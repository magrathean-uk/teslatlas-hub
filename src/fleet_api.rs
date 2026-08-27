//! Narrow Tesla Fleet REST wake and command client.
//!
//! Fleet REST calls use fixed regional Tesla endpoints. Vehicle commands are
//! sent only to an explicitly configured loopback instance of Tesla's command
//! proxy, which owns the vehicle-command signing protocol.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::IpAddr,
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{
    Client,
    header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    hub_pack::ProjectionCarSettings,
    owner_api::{StreamVehicleId, Vehicle, VehicleData, VehicleId},
};

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_ERROR_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_PROVIDER_ERROR_BYTES: usize = 64;
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_REASON_BYTES: usize = 1024;
const MAX_CA_CERTIFICATE_BYTES: usize = 128 * 1024;
const MAX_CLIENT_ID_BYTES: usize = 255;
const MAX_TELEMETRY_CONFIG_BODY_BYTES: usize = 128 * 1024;
const MAX_TELEMETRY_VINS: usize = 100;
const MAX_TELEMETRY_FIELDS: usize = 64;
const MAX_TELEMETRY_HOSTNAME_BYTES: usize = 253;
const MAX_TELEMETRY_FIELD_INTERVAL_SECONDS: u32 = 365 * 24 * 60 * 60;
const FLEET_TELEMETRY_DELIVERY_POLICY: &str = "latest";
const MIN_TOKEN_LIFETIME_SECONDS: u64 = 60;
const MAX_TOKEN_LIFETIME_SECONDS: u64 = 365 * 24 * 60 * 60;
const ACCEPT_JSON: HeaderValue = HeaderValue::from_static("application/json");
const VEHICLE_DATA_ENDPOINTS: &str = "location_data;charge_state;climate_state;drive_state;gui_settings;vehicle_config;vehicle_state";
const CONTENT_TYPE_JSON: HeaderValue = HeaderValue::from_static("application/json");
const CONTENT_TYPE_FORM: HeaderValue =
    HeaderValue::from_static("application/x-www-form-urlencoded");

/// Tesla's documented Fleet REST regions.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FleetRegion {
    NorthAmericaAndAsiaPacific,
    EuropeMiddleEastAndAfrica,
    China,
}

impl FleetRegion {
    pub const fn base_url(self) -> &'static str {
        match self {
            Self::NorthAmericaAndAsiaPacific => "https://fleet-api.prd.na.vn.cloud.tesla.com/",
            Self::EuropeMiddleEastAndAfrica => "https://fleet-api.prd.eu.vn.cloud.tesla.com/",
            Self::China => "https://fleet-api.prd.cn.vn.cloud.tesla.cn/",
        }
    }

    pub const fn storage_code(self) -> &'static str {
        match self {
            Self::NorthAmericaAndAsiaPacific => "na",
            Self::EuropeMiddleEastAndAfrica => "eu",
            Self::China => "cn",
        }
    }

    pub const fn auth_token_url(self) -> &'static str {
        match self {
            Self::NorthAmericaAndAsiaPacific | Self::EuropeMiddleEastAndAfrica => {
                "https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token"
            }
            Self::China => "https://auth.tesla.cn/oauth2/v3/token",
        }
    }

    pub fn from_storage_code(value: &str) -> Result<Self, FleetApiConfigError> {
        match value {
            "na" => Ok(Self::NorthAmericaAndAsiaPacific),
            "eu" => Ok(Self::EuropeMiddleEastAndAfrica),
            "cn" => Ok(Self::China),
            _ => Err(FleetApiConfigError::InvalidRegion),
        }
    }
}

/// One Fleet bearer. It cannot be cloned and never prints its contents.
#[derive(PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct FleetAccessToken(Zeroizing<String>);

impl FleetAccessToken {
    pub fn new(value: impl Into<String>) -> Result<Self, FleetApiConfigError> {
        let value = Zeroizing::new(value.into());
        if value.is_empty()
            || value.len() > MAX_TOKEN_BYTES
            || value
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(FleetApiConfigError::InvalidAccessToken);
        }
        Ok(Self(value))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for FleetAccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FleetAccessToken([redacted])")
    }
}

#[derive(PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct FleetRefreshToken(Zeroizing<String>);

impl FleetRefreshToken {
    pub fn new(value: impl Into<String>) -> Result<Self, FleetApiConfigError> {
        let value = Zeroizing::new(value.into());
        if value.is_empty()
            || value.len() > MAX_TOKEN_BYTES
            || value
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(FleetApiConfigError::InvalidRefreshToken);
        }
        Ok(Self(value))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for FleetRefreshToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FleetRefreshToken([redacted])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct FleetClientId(String);

impl FleetClientId {
    pub fn parse(value: &str) -> Result<Self, FleetApiConfigError> {
        if value.is_empty()
            || value.len() > MAX_CLIENT_ID_BYTES
            || value
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(FleetApiConfigError::InvalidClientId);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for FleetClientId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FleetClientId([redacted])")
    }
}

pub struct FleetRefreshedTokens {
    pub access_token: FleetAccessToken,
    pub refresh_token: FleetRefreshToken,
    pub expires_in_seconds: u64,
}

impl fmt::Debug for FleetRefreshedTokens {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FleetRefreshedTokens")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("expires_in_seconds", &self.expires_in_seconds)
            .finish()
    }
}

#[derive(Clone)]
pub struct FleetAuthApi {
    client: Client,
    endpoint: Url,
}

impl FleetAuthApi {
    pub fn new(
        region: FleetRegion,
        request_timeout: Duration,
    ) -> Result<Self, FleetApiConfigError> {
        let endpoint = Url::parse(region.auth_token_url()).expect("fixed Fleet auth URL");
        Self::build(endpoint, request_timeout, false)
    }

    fn build(
        endpoint: Url,
        request_timeout: Duration,
        allow_insecure_loopback: bool,
    ) -> Result<Self, FleetApiConfigError> {
        if request_timeout.is_zero() {
            return Err(FleetApiConfigError::ZeroTimeout);
        }
        if endpoint.scheme() != "https" && !(allow_insecure_loopback && is_loopback_http(&endpoint))
        {
            return Err(FleetApiConfigError::HttpsRequired);
        }
        if endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(FleetApiConfigError::InvalidAuthUrl);
        }
        crate::crypto::install_default_provider();
        let client = Client::builder()
            .https_only(!allow_insecure_loopback)
            .redirect(Policy::none())
            .timeout(request_timeout)
            .build()
            .map_err(|_| FleetApiConfigError::ClientBuild)?;
        Ok(Self { client, endpoint })
    }

    pub async fn refresh(
        &self,
        client_id: &FleetClientId,
        refresh_token: &FleetRefreshToken,
    ) -> Result<FleetRefreshedTokens, FleetApiError> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("client_id", client_id.as_str())
            .append_pair("refresh_token", refresh_token.expose())
            .finish();
        let response: FleetTokenWire = execute_json(
            self.client
                .post(self.endpoint.clone())
                .header(ACCEPT, ACCEPT_JSON.clone())
                .header(CONTENT_TYPE, CONTENT_TYPE_FORM.clone())
                .body(body),
        )
        .await?;
        if !response.token_type.eq_ignore_ascii_case("bearer")
            || !(MIN_TOKEN_LIFETIME_SECONDS..=MAX_TOKEN_LIFETIME_SECONDS)
                .contains(&response.expires_in)
        {
            return Err(FleetApiError::InvalidResponse);
        }
        Ok(FleetRefreshedTokens {
            access_token: FleetAccessToken::new(response.access_token)
                .map_err(|_| FleetApiError::InvalidResponse)?,
            refresh_token: FleetRefreshToken::new(response.refresh_token)
                .map_err(|_| FleetApiError::InvalidResponse)?,
            expires_in_seconds: response.expires_in,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_fake_http(
        endpoint: Url,
        request_timeout: Duration,
    ) -> Result<Self, FleetApiConfigError> {
        Self::build(endpoint, request_timeout, true)
    }
}

impl fmt::Debug for FleetAuthApi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FleetAuthApi")
            .field("endpoint", &"[fixed-or-loopback]")
            .field("redirects", &"disabled")
            .finish()
    }
}

/// A standard, path-safe vehicle identification number.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub struct VehicleVin(String);

impl VehicleVin {
    pub fn parse(value: &str) -> Result<Self, FleetApiConfigError> {
        if value.len() != 17 {
            return Err(FleetApiConfigError::InvalidVin);
        }
        let value = value.to_ascii_uppercase();
        if !value.bytes().all(|byte| byte.is_ascii_alphanumeric())
            || value.bytes().any(|byte| matches!(byte, b'I' | b'O' | b'Q'))
        {
            return Err(FleetApiConfigError::InvalidVin);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for VehicleVin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("VehicleVin").field(&self.0).finish()
    }
}

impl fmt::Display for VehicleVin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Explicit command allowlist. No arbitrary endpoint or body is accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetCommand {
    ClimateStart,
    ClimateStop,
    Lock,
    Unlock,
    ChargeStart,
    ChargeStop,
    SetChargeLimit { percent: u8 },
    FlashLights,
    HonkHorn,
}

/// Fields accepted by the Fleet Telemetry configuration contract.
///
/// Keep this list explicit: the proxy signs the resulting configuration, so
/// callers must not be able to smuggle arbitrary claims into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum FleetTelemetryField {
    #[serde(rename = "Location")]
    Location,
    #[serde(rename = "VehicleSpeed")]
    VehicleSpeed,
    #[serde(rename = "Gear")]
    Gear,
    #[serde(rename = "GpsHeading")]
    GpsHeading,
    #[serde(rename = "PackVoltage")]
    PackVoltage,
    #[serde(rename = "PackCurrent")]
    PackCurrent,
    #[serde(rename = "Soc")]
    Soc,
    #[serde(rename = "BatteryLevel")]
    BatteryLevel,
    #[serde(rename = "RatedRange")]
    RatedRange,
    #[serde(rename = "EstBatteryRange")]
    EstBatteryRange,
    #[serde(rename = "IdealBatteryRange")]
    IdealBatteryRange,
    #[serde(rename = "DetailedChargeState")]
    DetailedChargeState,
    #[serde(rename = "DCChargingEnergyIn")]
    DcChargingEnergyIn,
    #[serde(rename = "ACChargingPower")]
    AcChargingPower,
    #[serde(rename = "DCChargingPower")]
    DcChargingPower,
    #[serde(rename = "ChargeAmps")]
    ChargeAmps,
    #[serde(rename = "ChargeLimitSoc")]
    ChargeLimitSoc,
    #[serde(rename = "ChargePortDoorOpen")]
    ChargePortDoorOpen,
    #[serde(rename = "Odometer")]
    Odometer,
    #[serde(rename = "InsideTemp")]
    InsideTemp,
    #[serde(rename = "OutsideTemp")]
    OutsideTemp,
    #[serde(rename = "HvacPower")]
    HvacPower,
    #[serde(rename = "HvacFanStatus")]
    HvacFanStatus,
    #[serde(rename = "HvacLeftTemperatureRequest")]
    HvacLeftTemperatureRequest,
    #[serde(rename = "HvacRightTemperatureRequest")]
    HvacRightTemperatureRequest,
    #[serde(rename = "PreconditioningEnabled")]
    PreconditioningEnabled,
    #[serde(rename = "Locked")]
    Locked,
    #[serde(rename = "SentryMode")]
    SentryMode,
    #[serde(rename = "ServiceMode")]
    ServiceMode,
    #[serde(rename = "DoorState")]
    DoorState,
    #[serde(rename = "FdWindow")]
    FdWindow,
    #[serde(rename = "RdWindow")]
    RdWindow,
    #[serde(rename = "FpWindow")]
    FpWindow,
    #[serde(rename = "RpWindow")]
    RpWindow,
    #[serde(rename = "TpmsPressureFl")]
    TpmsPressureFl,
    #[serde(rename = "TpmsPressureFr")]
    TpmsPressureFr,
    #[serde(rename = "TpmsPressureRl")]
    TpmsPressureRl,
    #[serde(rename = "TpmsPressureRr")]
    TpmsPressureRr,
    #[serde(rename = "TpmsSoftWarnings")]
    TpmsSoftWarnings,
    #[serde(rename = "Version")]
    Version,
    #[serde(rename = "CarType")]
    CarType,
    #[serde(rename = "Trim")]
    Trim,
    #[serde(rename = "ExteriorColor")]
    ExteriorColor,
    #[serde(rename = "WheelType")]
    WheelType,
    #[serde(rename = "SoftwareUpdateVersion")]
    SoftwareUpdateVersion,
    #[serde(rename = "SoftwareUpdateDownloadPercentComplete")]
    SoftwareUpdateDownloadPercentComplete,
    #[serde(rename = "SoftwareUpdateInstallationPercentComplete")]
    SoftwareUpdateInstallationPercentComplete,
}

/// One bounded Fleet Telemetry field policy.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct FleetTelemetryFieldConfig {
    pub interval_seconds: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_delta: Option<f64>,
}

impl FleetTelemetryFieldConfig {
    pub fn new(interval_seconds: u32) -> Result<Self, FleetApiConfigError> {
        if !(1..=MAX_TELEMETRY_FIELD_INTERVAL_SECONDS).contains(&interval_seconds) {
            return Err(FleetApiConfigError::InvalidTelemetryFieldInterval);
        }
        Ok(Self {
            interval_seconds,
            minimum_delta: None,
        })
    }

    pub fn with_minimum_delta(mut self, minimum_delta: f64) -> Result<Self, FleetApiConfigError> {
        if !minimum_delta.is_finite() || minimum_delta <= 0.0 {
            return Err(FleetApiConfigError::InvalidTelemetryMinimumDelta);
        }
        self.minimum_delta = Some(minimum_delta);
        Ok(self)
    }
}

/// A validated Fleet Telemetry destination and field selection.
#[derive(Clone, PartialEq, Serialize)]
pub struct FleetTelemetryConfig {
    hostname: String,
    port: u16,
    ca: String,
    #[serde(rename = "exp")]
    expiration: u64,
    #[serde(rename = "delivery_policy")]
    delivery_policy: &'static str,
    fields: BTreeMap<FleetTelemetryField, FleetTelemetryFieldConfig>,
}

impl fmt::Debug for FleetTelemetryConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FleetTelemetryConfig")
            .field("hostname", &self.hostname)
            .field("port", &self.port)
            .field("ca", &"[redacted]")
            .field("expiration", &self.expiration)
            .field("delivery_policy", &self.delivery_policy)
            .field("fields", &self.fields)
            .finish()
    }
}

impl FleetTelemetryConfig {
    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    pub const fn port(&self) -> u16 {
        self.port
    }

    pub const fn expiration(&self) -> u64 {
        self.expiration
    }

    pub fn fields(&self) -> &BTreeMap<FleetTelemetryField, FleetTelemetryFieldConfig> {
        &self.fields
    }
}

/// Builder for the fixed Fleet Telemetry request contract.
#[derive(Clone, Default)]
pub struct FleetTelemetryConfigBuilder {
    hostname: Option<String>,
    port: Option<u16>,
    ca: Option<String>,
    expiration: Option<u64>,
    fields: BTreeMap<FleetTelemetryField, FleetTelemetryFieldConfig>,
}

impl fmt::Debug for FleetTelemetryConfigBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FleetTelemetryConfigBuilder")
            .field("hostname", &self.hostname)
            .field("port", &self.port)
            .field("ca", &self.ca.as_ref().map(|_| "[redacted]"))
            .field("expiration", &self.expiration)
            .field("fields", &self.fields)
            .finish()
    }
}

impl FleetTelemetryConfigBuilder {
    pub fn new(
        hostname: impl Into<String>,
        port: u16,
        ca: impl Into<String>,
        expiration: u64,
    ) -> Self {
        Self {
            hostname: Some(hostname.into()),
            port: Some(port),
            ca: Some(ca.into()),
            expiration: Some(expiration),
            fields: BTreeMap::new(),
        }
    }

    pub fn field(mut self, field: FleetTelemetryField, config: FleetTelemetryFieldConfig) -> Self {
        self.fields.insert(field, config);
        self
    }

    pub fn fields<I>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = (FleetTelemetryField, FleetTelemetryFieldConfig)>,
    {
        self.fields.extend(fields);
        self
    }

    /// The conservative field set used by the cost-focused setup path.
    pub fn recommended_fields() -> BTreeMap<FleetTelemetryField, FleetTelemetryFieldConfig> {
        [
            (FleetTelemetryField::Location, 5, None),
            (FleetTelemetryField::VehicleSpeed, 5, None),
            (FleetTelemetryField::Gear, 1, None),
            (FleetTelemetryField::GpsHeading, 5, None),
            (FleetTelemetryField::PackVoltage, 5, None),
            (FleetTelemetryField::PackCurrent, 5, None),
            (FleetTelemetryField::Soc, 30, None),
            (FleetTelemetryField::BatteryLevel, 30, None),
            (FleetTelemetryField::RatedRange, 30, None),
            (FleetTelemetryField::EstBatteryRange, 30, None),
            (FleetTelemetryField::IdealBatteryRange, 30, None),
            (FleetTelemetryField::DetailedChargeState, 1, None),
            (FleetTelemetryField::DcChargingEnergyIn, 15, None),
            (FleetTelemetryField::AcChargingPower, 15, None),
            (FleetTelemetryField::DcChargingPower, 15, None),
            (FleetTelemetryField::ChargeAmps, 15, None),
            (FleetTelemetryField::ChargeLimitSoc, 30, None),
            (FleetTelemetryField::ChargePortDoorOpen, 1, None),
            (FleetTelemetryField::Odometer, 60, None),
            (FleetTelemetryField::InsideTemp, 60, None),
            (FleetTelemetryField::OutsideTemp, 60, None),
            (FleetTelemetryField::HvacPower, 60, None),
            (FleetTelemetryField::HvacFanStatus, 60, None),
            (FleetTelemetryField::HvacLeftTemperatureRequest, 60, None),
            (FleetTelemetryField::HvacRightTemperatureRequest, 60, None),
            (FleetTelemetryField::PreconditioningEnabled, 60, None),
            (FleetTelemetryField::Locked, 1, None),
            (FleetTelemetryField::SentryMode, 5, None),
            (FleetTelemetryField::ServiceMode, 5, None),
            (FleetTelemetryField::DoorState, 1, None),
            (FleetTelemetryField::FdWindow, 1, None),
            (FleetTelemetryField::RdWindow, 1, None),
            (FleetTelemetryField::FpWindow, 1, None),
            (FleetTelemetryField::RpWindow, 1, None),
            (FleetTelemetryField::TpmsPressureFl, 300, None),
            (FleetTelemetryField::TpmsPressureFr, 300, None),
            (FleetTelemetryField::TpmsPressureRl, 300, None),
            (FleetTelemetryField::TpmsPressureRr, 300, None),
            (FleetTelemetryField::TpmsSoftWarnings, 300, None),
            (FleetTelemetryField::Version, 3600, None),
            (FleetTelemetryField::CarType, 3600, None),
            (FleetTelemetryField::Trim, 3600, None),
            (FleetTelemetryField::ExteriorColor, 3600, None),
            (FleetTelemetryField::WheelType, 3600, None),
            (FleetTelemetryField::SoftwareUpdateVersion, 300, None),
            (
                FleetTelemetryField::SoftwareUpdateDownloadPercentComplete,
                60,
                None,
            ),
            (
                FleetTelemetryField::SoftwareUpdateInstallationPercentComplete,
                60,
                None,
            ),
        ]
        .into_iter()
        .map(|(field, interval, minimum_delta)| {
            (
                field,
                FleetTelemetryFieldConfig {
                    interval_seconds: interval,
                    minimum_delta,
                },
            )
        })
        .collect()
    }

    pub fn with_recommended_fields(mut self) -> Self {
        self.fields = Self::recommended_fields();
        self
    }

    pub fn build(self) -> Result<FleetTelemetryConfig, FleetApiConfigError> {
        let hostname = self
            .hostname
            .ok_or(FleetApiConfigError::InvalidTelemetryHostname)?;
        validate_telemetry_hostname(&hostname)?;
        let port = self.port.ok_or(FleetApiConfigError::InvalidTelemetryPort)?;
        if port == 0 {
            return Err(FleetApiConfigError::InvalidTelemetryPort);
        }
        let ca = self.ca.ok_or(FleetApiConfigError::InvalidTelemetryCa)?;
        validate_telemetry_ca(&ca)?;
        let expiration = self
            .expiration
            .ok_or(FleetApiConfigError::InvalidTelemetryExpiration)?;
        if expiration == 0 || expiration > i64::MAX as u64 {
            return Err(FleetApiConfigError::InvalidTelemetryExpiration);
        }
        if self.fields.is_empty() || self.fields.len() > MAX_TELEMETRY_FIELDS {
            return Err(FleetApiConfigError::InvalidTelemetryFields);
        }
        for (field, config) in &self.fields {
            FleetTelemetryFieldConfig::new(config.interval_seconds)?;
            if let Some(minimum_delta) = config.minimum_delta
                && (*field != FleetTelemetryField::Location
                    || !minimum_delta.is_finite()
                    || minimum_delta <= 0.0)
            {
                return Err(FleetApiConfigError::InvalidTelemetryMinimumDelta);
            }
        }
        Ok(FleetTelemetryConfig {
            hostname,
            port,
            ca,
            expiration,
            delivery_policy: FLEET_TELEMETRY_DELIVERY_POLICY,
            fields: self.fields,
        })
    }
}

/// A validated list of VINs targeted by one signed configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FleetTelemetryVins(Vec<VehicleVin>);

impl FleetTelemetryVins {
    pub fn new<I>(vins: I) -> Result<Self, FleetApiConfigError>
    where
        I: IntoIterator<Item = VehicleVin>,
    {
        let mut values = Vec::new();
        for vin in vins {
            if values.len() >= MAX_TELEMETRY_VINS {
                return Err(FleetApiConfigError::InvalidTelemetryVins);
            }
            if values.iter().any(|existing| existing == &vin) {
                return Err(FleetApiConfigError::DuplicateTelemetryVin);
            }
            values.push(vin);
        }
        if values.is_empty() {
            return Err(FleetApiConfigError::InvalidTelemetryVins);
        }
        Ok(Self(values))
    }

    pub fn parse<I, S>(vins: I) -> Result<Self, FleetApiConfigError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut values = Vec::new();
        for vin in vins {
            if values.len() >= MAX_TELEMETRY_VINS {
                return Err(FleetApiConfigError::InvalidTelemetryVins);
            }
            let vin = VehicleVin::parse(vin.as_ref())?;
            if values.iter().any(|existing| existing == &vin) {
                return Err(FleetApiConfigError::DuplicateTelemetryVin);
            }
            values.push(vin);
        }
        if values.is_empty() {
            return Err(FleetApiConfigError::InvalidTelemetryVins);
        }
        Ok(Self(values))
    }

    pub fn as_slice(&self) -> &[VehicleVin] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetTelemetrySkippedVehicle {
    pub vin: VehicleVin,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetTelemetryConfigureResult {
    pub updated_vehicles: usize,
    pub skipped_vehicles: Vec<FleetTelemetrySkippedVehicle>,
}

impl FleetCommand {
    fn endpoint(self) -> &'static str {
        match self {
            Self::ClimateStart => "auto_conditioning_start",
            Self::ClimateStop => "auto_conditioning_stop",
            Self::Lock => "door_lock",
            Self::Unlock => "door_unlock",
            Self::ChargeStart => "charge_start",
            Self::ChargeStop => "charge_stop",
            Self::SetChargeLimit { .. } => "set_charge_limit",
            Self::FlashLights => "flash_lights",
            Self::HonkHorn => "honk_horn",
        }
    }

    fn body(self) -> Result<Vec<u8>, FleetApiError> {
        match self {
            Self::SetChargeLimit { percent } if !(50..=100).contains(&percent) => {
                Err(FleetApiError::InvalidChargeLimit)
            }
            Self::SetChargeLimit { percent } => serde_json::to_vec(&ChargeLimitBody { percent })
                .map_err(|_| FleetApiError::InvalidCommand),
            _ => Ok(b"{}".to_vec()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeResult {
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetCommandResult {
    pub result: bool,
    pub reason: Option<String>,
}

/// Direct Fleet REST client. Its base cannot be configured in production.
#[derive(Clone)]
pub struct FleetApi {
    client: Client,
    base_url: Url,
}

impl FleetApi {
    pub fn new(
        region: FleetRegion,
        request_timeout: Duration,
    ) -> Result<Self, FleetApiConfigError> {
        let base_url = Url::parse(region.base_url())
            .expect("fixed Tesla Fleet API base URLs are valid absolute URLs");
        Self::build(base_url, request_timeout, false)
    }

    fn build(
        base_url: Url,
        request_timeout: Duration,
        allow_insecure_loopback: bool,
    ) -> Result<Self, FleetApiConfigError> {
        if request_timeout.is_zero() {
            return Err(FleetApiConfigError::ZeroTimeout);
        }
        if base_url.scheme() != "https" && !(allow_insecure_loopback && is_loopback_http(&base_url))
        {
            return Err(FleetApiConfigError::HttpsRequired);
        }
        validate_base_url(&base_url, false)?;
        crate::crypto::install_default_provider();
        let client = Client::builder()
            .https_only(!allow_insecure_loopback)
            .redirect(Policy::none())
            .timeout(request_timeout)
            .build()
            .map_err(|_| FleetApiConfigError::ClientBuild)?;
        Ok(Self { client, base_url })
    }

    pub async fn wake(
        &self,
        access_token: &FleetAccessToken,
        vin: &VehicleVin,
    ) -> Result<WakeResult, FleetApiError> {
        let endpoint = endpoint(&self.base_url, &format!("api/1/vehicles/{vin}/wake_up"))?;
        let envelope: ResponseEnvelope<WakeWire> = execute_json(
            self.client
                .post(endpoint)
                .header(ACCEPT, ACCEPT_JSON.clone())
                .bearer_auth(access_token.expose()),
        )
        .await?;
        if !valid_short_text(&envelope.response.state) {
            return Err(FleetApiError::InvalidResponse);
        }
        Ok(WakeResult {
            state: envelope.response.state,
        })
    }

    pub async fn list_vehicles(
        &self,
        access_token: &FleetAccessToken,
    ) -> Result<Vec<Vehicle>, FleetApiError> {
        let endpoint = endpoint(&self.base_url, "api/1/vehicles")?;
        let envelope: ResponseEnvelope<Vec<FleetVehicleWire>> = execute_json(
            self.client
                .get(endpoint)
                .header(ACCEPT, ACCEPT_JSON.clone())
                .bearer_auth(access_token.expose()),
        )
        .await?;
        if envelope
            .count
            .is_some_and(|count| count != envelope.response.len())
        {
            return Err(FleetApiError::InvalidResponse);
        }
        envelope
            .response
            .into_iter()
            .map(FleetVehicleWire::into_vehicle)
            .collect()
    }

    pub async fn vehicle_data(
        &self,
        access_token: &FleetAccessToken,
        vehicle_id: VehicleId,
        vin: &VehicleVin,
    ) -> Result<VehicleData, FleetApiError> {
        let endpoint = endpoint(
            &self.base_url,
            &format!("api/1/vehicles/{vin}/vehicle_data"),
        )?;
        let raw_json: Value = execute_json(
            self.client
                .get(endpoint)
                .query(&[("endpoints", VEHICLE_DATA_ENDPOINTS)])
                .header(ACCEPT, ACCEPT_JSON.clone())
                .bearer_auth(access_token.expose()),
        )
        .await?;
        VehicleData::from_provider_raw_json(vehicle_id, raw_json)
            .map_err(|_| FleetApiError::InvalidResponse)
    }

    #[cfg(test)]
    pub(crate) fn for_fake_http(
        base_url: Url,
        request_timeout: Duration,
    ) -> Result<Self, FleetApiConfigError> {
        Self::build(base_url, request_timeout, true)
    }
}

impl fmt::Debug for FleetApi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FleetApi")
            .field("base_url", &self.base_url)
            .field("redirects", &"disabled")
            .finish()
    }
}

/// Validated local Tesla command-proxy base.
#[derive(Clone, PartialEq, Eq)]
pub struct FleetCommandProxyBase {
    url: Url,
}

impl FleetCommandProxyBase {
    pub fn parse(value: &str) -> Result<Self, FleetApiConfigError> {
        let url = Url::parse(value).map_err(|_| FleetApiConfigError::InvalidProxyUrl)?;
        Self::from_url(url, false)
    }

    fn from_url(mut url: Url, allow_insecure_loopback: bool) -> Result<Self, FleetApiConfigError> {
        validate_base_url(&url, true)?;
        if url.scheme() != "https" && !(allow_insecure_loopback && is_loopback_http(&url)) {
            return Err(FleetApiConfigError::HttpsRequired);
        }
        if !is_loopback_host(url.host_str()) {
            return Err(FleetApiConfigError::ProxyMustBeLoopback);
        }
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        Ok(Self { url })
    }

    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    #[cfg(test)]
    pub(crate) fn parse_loopback_http_for_test(value: &str) -> Result<Self, FleetApiConfigError> {
        let url = Url::parse(value).map_err(|_| FleetApiConfigError::InvalidProxyUrl)?;
        Self::from_url(url, true)
    }
}

impl fmt::Debug for FleetCommandProxyBase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("FleetCommandProxyBase")
            .field(&"[loopback]")
            .finish()
    }
}

#[derive(Clone)]
pub struct FleetCommandProxy {
    client: Client,
    base_url: FleetCommandProxyBase,
}

impl FleetCommandProxy {
    pub fn new(
        base_url: FleetCommandProxyBase,
        request_timeout: Duration,
        root_certificate_pem: Option<&[u8]>,
    ) -> Result<Self, FleetApiConfigError> {
        Self::build(base_url, request_timeout, root_certificate_pem, false)
    }

    fn build(
        base_url: FleetCommandProxyBase,
        request_timeout: Duration,
        root_certificate_pem: Option<&[u8]>,
        allow_insecure_loopback: bool,
    ) -> Result<Self, FleetApiConfigError> {
        if request_timeout.is_zero() {
            return Err(FleetApiConfigError::ZeroTimeout);
        }
        if base_url.url.scheme() != "https" && !allow_insecure_loopback {
            return Err(FleetApiConfigError::HttpsRequired);
        }
        crate::crypto::install_default_provider();
        let client = command_proxy_client_builder(
            request_timeout,
            root_certificate_pem,
            allow_insecure_loopback,
        )?
        .build()
        .map_err(|_| FleetApiConfigError::ClientBuild)?;
        Ok(Self { client, base_url })
    }

    pub async fn execute(
        &self,
        access_token: &FleetAccessToken,
        vin: &VehicleVin,
        command: FleetCommand,
    ) -> Result<FleetCommandResult, FleetApiError> {
        let body = command.body()?;
        let endpoint = endpoint(
            &self.base_url.url,
            &format!("api/1/vehicles/{vin}/command/{}", command.endpoint()),
        )?;
        let envelope: ResponseEnvelope<CommandWire> = execute_json(
            self.client
                .post(endpoint)
                .header(ACCEPT, ACCEPT_JSON.clone())
                .header(CONTENT_TYPE, CONTENT_TYPE_JSON.clone())
                .bearer_auth(access_token.expose())
                .body(body),
        )
        .await?;
        let reason = envelope.response.reason.filter(|reason| !reason.is_empty());
        if reason.as_deref().is_some_and(|reason| {
            reason.len() > MAX_REASON_BYTES || reason.chars().any(char::is_control)
        }) {
            return Err(FleetApiError::InvalidResponse);
        }
        if !envelope.response.result {
            return Err(FleetApiError::CommandRejected);
        }
        Ok(FleetCommandResult {
            result: envelope.response.result,
            reason,
        })
    }

    /// Configure Fleet Telemetry through the proxy's signing path.
    ///
    /// The proxy signs this fixed request before forwarding it to Tesla. The
    /// request intentionally accepts only the typed destination and field
    /// policy above; callers cannot select an arbitrary proxy endpoint or add
    /// signing claims.
    pub async fn configure_fleet_telemetry(
        &self,
        access_token: &FleetAccessToken,
        vins: &FleetTelemetryVins,
        config: &FleetTelemetryConfig,
    ) -> Result<FleetTelemetryConfigureResult, FleetApiError> {
        let request = FleetTelemetryConfigRequest { vins, config };
        let body =
            serde_json::to_vec(&request).map_err(|_| FleetApiError::InvalidTelemetryConfig)?;
        if body.len() > MAX_TELEMETRY_CONFIG_BODY_BYTES {
            return Err(FleetApiError::TelemetryConfigTooLarge);
        }
        let endpoint =
            fixed_proxy_endpoint(&self.base_url.url, "/api/1/vehicles/fleet_telemetry_config")?;
        let envelope: ResponseEnvelope<FleetTelemetryConfigureWire> = execute_json(
            self.client
                .post(endpoint)
                .header(ACCEPT, ACCEPT_JSON.clone())
                .header(CONTENT_TYPE, CONTENT_TYPE_JSON.clone())
                .bearer_auth(access_token.expose())
                .body(body),
        )
        .await?;
        envelope.response.into_result(vins)
    }

    /// Remove this application's Fleet Telemetry configuration from one
    /// validated vehicle through the proxy's signing path.
    pub async fn remove_fleet_telemetry(
        &self,
        access_token: &FleetAccessToken,
        vin: &VehicleVin,
    ) -> Result<(), FleetApiError> {
        let path = format!("/api/1/vehicles/{vin}/fleet_telemetry_config");
        let endpoint = fixed_proxy_endpoint(&self.base_url.url, &path)?;
        let _: ResponseEnvelope<Value> = execute_json(
            self.client
                .delete(endpoint)
                .header(ACCEPT, ACCEPT_JSON.clone())
                .bearer_auth(access_token.expose()),
        )
        .await?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn for_fake_http(
        base_url: FleetCommandProxyBase,
        request_timeout: Duration,
    ) -> Result<Self, FleetApiConfigError> {
        Self::build(base_url, request_timeout, None, true)
    }
}

fn command_proxy_client_builder(
    request_timeout: Duration,
    root_certificate_pem: Option<&[u8]>,
    allow_insecure_loopback: bool,
) -> Result<reqwest::ClientBuilder, FleetApiConfigError> {
    let builder = Client::builder()
        .https_only(!allow_insecure_loopback)
        .redirect(Policy::none())
        .timeout(request_timeout);
    let Some(pem) = root_certificate_pem else {
        return Ok(builder);
    };
    if pem.is_empty() || pem.len() > MAX_CA_CERTIFICATE_BYTES {
        return Err(FleetApiConfigError::InvalidRootCertificate);
    }
    let certificate = reqwest::Certificate::from_pem(pem)
        .map_err(|_| FleetApiConfigError::InvalidRootCertificate)?;
    Ok(builder.tls_certs_only([certificate]))
}

impl fmt::Debug for FleetCommandProxy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FleetCommandProxy")
            .field("base_url", &self.base_url)
            .field("redirects", &"disabled")
            .finish()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FleetApiConfigError {
    #[error("Fleet access token is invalid")]
    InvalidAccessToken,
    #[error("Fleet refresh token is invalid")]
    InvalidRefreshToken,
    #[error("Fleet client id is invalid")]
    InvalidClientId,
    #[error("Fleet authentication endpoint is invalid")]
    InvalidAuthUrl,
    #[error("vehicle VIN is invalid")]
    InvalidVin,
    #[error("Fleet region is invalid")]
    InvalidRegion,
    #[error("Fleet command proxy URL is invalid")]
    InvalidProxyUrl,
    #[error("Fleet endpoint must use HTTPS")]
    HttpsRequired,
    #[error("Fleet command proxy must use a loopback host")]
    ProxyMustBeLoopback,
    #[error("Fleet endpoint requires a host")]
    HostRequired,
    #[error("Fleet endpoint cannot contain credentials")]
    EmbeddedCredential,
    #[error("Fleet endpoint cannot contain query parameters or a fragment")]
    ParametersNotPermitted,
    #[error("Fleet endpoint cannot contain path traversal")]
    PathTraversal,
    #[error("Fleet request timeout must be greater than zero")]
    ZeroTimeout,
    #[error("Fleet command proxy root certificate is invalid")]
    InvalidRootCertificate,
    #[error("Fleet Telemetry hostname is invalid")]
    InvalidTelemetryHostname,
    #[error("Fleet Telemetry port is invalid")]
    InvalidTelemetryPort,
    #[error("Fleet Telemetry CA certificate is invalid")]
    InvalidTelemetryCa,
    #[error("Fleet Telemetry expiration is invalid")]
    InvalidTelemetryExpiration,
    #[error("Fleet Telemetry field selection is invalid")]
    InvalidTelemetryFields,
    #[error("Fleet Telemetry field interval is invalid")]
    InvalidTelemetryFieldInterval,
    #[error("Fleet Telemetry minimum delta is invalid")]
    InvalidTelemetryMinimumDelta,
    #[error("Fleet Telemetry VIN list is invalid")]
    InvalidTelemetryVins,
    #[error("Fleet Telemetry VIN list contains a duplicate")]
    DuplicateTelemetryVin,
    #[error("Fleet HTTP client could not be constructed")]
    ClientBuild,
}

/// Errors retain no bearer, URL, request body, or raw response body.
/// Provider failures may retain only an allowlisted machine-readable error code.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FleetApiError {
    #[error("Fleet endpoint is invalid")]
    InvalidEndpoint,
    #[error("Fleet request timed out")]
    RequestTimeout,
    #[error("Fleet request could not be sent")]
    RequestNotSent,
    #[error("Fleet transport failed")]
    Transport,
    #[error("Fleet endpoint returned HTTP {0}")]
    HttpStatus(u16),
    #[error("Fleet endpoint returned HTTP {status}: {error}")]
    ProviderHttpStatus {
        status: u16,
        error: String,
        description: Option<String>,
    },
    #[error("Fleet endpoint rate limited; retry after {retry_after_seconds}s")]
    RateLimited { retry_after_seconds: u64 },
    #[error("Fleet response exceeds the size limit")]
    ResponseTooLarge,
    #[error("Fleet response body could not be read")]
    ResponseRead,
    #[error("Fleet response is invalid")]
    InvalidResponse,
    #[error("Fleet command is invalid")]
    InvalidCommand,
    #[error("Fleet command was rejected")]
    CommandRejected,
    #[error("Fleet Telemetry configuration is invalid")]
    InvalidTelemetryConfig,
    #[error("Fleet Telemetry configuration exceeds the size limit")]
    TelemetryConfigTooLarge,
    #[error("Fleet command proxy is not configured")]
    CommandProxyUnavailable,
    #[error("charge limit must be between 50 and 100 percent")]
    InvalidChargeLimit,
}

impl FleetApiError {
    pub const fn http_status(&self) -> Option<u16> {
        match self {
            Self::HttpStatus(status) | Self::ProviderHttpStatus { status, .. } => Some(*status),
            Self::RateLimited { .. } => Some(429),
            _ => None,
        }
    }

    pub fn provider_error(&self) -> Option<(&str, Option<&str>)> {
        match self {
            Self::ProviderHttpStatus {
                error, description, ..
            } => Some((error, description.as_deref())),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct ResponseEnvelope<T> {
    response: T,
    #[serde(default)]
    count: Option<usize>,
}

#[derive(Deserialize)]
struct ProviderErrorEnvelope {
    error: String,
    #[serde(default, rename = "error_description")]
    _error_description: Option<String>,
}

#[derive(Deserialize)]
struct FleetVehicleWire {
    id: Value,
    vehicle_id: Value,
    vin: String,
    state: String,
    #[serde(default)]
    display_name: Option<String>,
}

impl FleetVehicleWire {
    fn into_vehicle(self) -> Result<Vehicle, FleetApiError> {
        let id = parse_numeric_id(&self.id)
            .and_then(|id| i64::try_from(id).ok())
            .and_then(VehicleId::try_from_i64)
            .ok_or(FleetApiError::InvalidResponse)?;
        let stream_id = parse_numeric_id(&self.vehicle_id)
            .and_then(|id| i64::try_from(id).ok())
            .and_then(StreamVehicleId::try_from_i64)
            .ok_or(FleetApiError::InvalidResponse)?;
        let vin = VehicleVin::parse(&self.vin).map_err(|_| FleetApiError::InvalidResponse)?;
        if !valid_short_text(&self.state)
            || self.display_name.as_deref().is_some_and(|name| {
                name.is_empty() || name.len() > 256 || name.chars().any(char::is_control)
            })
        {
            return Err(FleetApiError::InvalidResponse);
        }
        Ok(Vehicle {
            id,
            stream_id,
            vin: vin.to_string(),
            state: self.state,
            display_name: self.display_name,
            settings: ProjectionCarSettings::default(),
        })
    }
}

#[derive(Deserialize)]
struct WakeWire {
    state: String,
}

#[derive(Deserialize)]
struct CommandWire {
    result: bool,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Serialize)]
struct FleetTelemetryConfigRequest<'a> {
    vins: &'a FleetTelemetryVins,
    config: &'a FleetTelemetryConfig,
}

#[derive(Deserialize)]
struct FleetTelemetryConfigureWire {
    updated_vehicles: usize,
    #[serde(default)]
    skipped_vehicles: BTreeMap<String, Vec<String>>,
}

impl FleetTelemetryConfigureWire {
    fn into_result(
        self,
        requested: &FleetTelemetryVins,
    ) -> Result<FleetTelemetryConfigureResult, FleetApiError> {
        if self.updated_vehicles > requested.as_slice().len()
            || self.skipped_vehicles.len() > MAX_TELEMETRY_VINS
        {
            return Err(FleetApiError::InvalidResponse);
        }
        let mut skipped_vehicles = Vec::new();
        let mut seen = BTreeSet::new();
        for (reason, vins) in self.skipped_vehicles {
            if reason.is_empty()
                || reason.len() > MAX_REASON_BYTES
                || !reason
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            {
                return Err(FleetApiError::InvalidResponse);
            }
            for vin in vins {
                if skipped_vehicles.len() >= MAX_TELEMETRY_VINS {
                    return Err(FleetApiError::InvalidResponse);
                }
                let vin = VehicleVin::parse(&vin).map_err(|_| FleetApiError::InvalidResponse)?;
                if !requested.as_slice().contains(&vin) || !seen.insert(vin.to_string()) {
                    return Err(FleetApiError::InvalidResponse);
                }
                skipped_vehicles.push(FleetTelemetrySkippedVehicle {
                    vin,
                    reason: reason.clone(),
                });
            }
        }
        if self.updated_vehicles + skipped_vehicles.len() != requested.as_slice().len() {
            return Err(FleetApiError::InvalidResponse);
        }
        Ok(FleetTelemetryConfigureResult {
            updated_vehicles: self.updated_vehicles,
            skipped_vehicles,
        })
    }
}

#[derive(Deserialize)]
struct FleetTokenWire {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    token_type: String,
}

#[derive(Serialize)]
struct ChargeLimitBody {
    percent: u8,
}

fn validate_base_url(url: &Url, require_loopback: bool) -> Result<(), FleetApiConfigError> {
    if url.cannot_be_a_base() || url.host_str().is_none() {
        return Err(FleetApiConfigError::HostRequired);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FleetApiConfigError::EmbeddedCredential);
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(FleetApiConfigError::ParametersNotPermitted);
    }
    if url
        .path_segments()
        .is_some_and(|mut segments| segments.any(|segment| segment == ".."))
    {
        return Err(FleetApiConfigError::PathTraversal);
    }
    if require_loopback && !is_loopback_host(url.host_str()) {
        return Err(FleetApiConfigError::ProxyMustBeLoopback);
    }
    Ok(())
}

fn validate_telemetry_hostname(hostname: &str) -> Result<(), FleetApiConfigError> {
    if hostname.is_empty()
        || hostname.len() > MAX_TELEMETRY_HOSTNAME_BYTES
        || !hostname.is_ascii()
        || hostname.ends_with('.')
    {
        return Err(FleetApiConfigError::InvalidTelemetryHostname);
    }
    if hostname.parse::<IpAddr>().is_ok() || hostname.contains(':') || hostname.contains('/') {
        return Err(FleetApiConfigError::InvalidTelemetryHostname);
    }
    for label in hostname.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(FleetApiConfigError::InvalidTelemetryHostname);
        }
    }
    Ok(())
}

fn validate_telemetry_ca(ca: &str) -> Result<(), FleetApiConfigError> {
    if ca.is_empty() || ca.len() > MAX_CA_CERTIFICATE_BYTES {
        return Err(FleetApiConfigError::InvalidTelemetryCa);
    }
    use rustls::pki_types::{CertificateDer, pem::PemObject};

    let certificates = CertificateDer::pem_slice_iter(ca.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| FleetApiConfigError::InvalidTelemetryCa)?;
    if certificates.is_empty() {
        return Err(FleetApiConfigError::InvalidTelemetryCa);
    }
    let mut roots = rustls::RootCertStore::empty();
    for certificate in certificates {
        roots
            .add(certificate)
            .map_err(|_| FleetApiConfigError::InvalidTelemetryCa)?;
    }
    Ok(())
}

fn is_loopback_http(url: &Url) -> bool {
    url.scheme() == "http" && is_loopback_host(url.host_str())
}

fn is_loopback_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
        || host
            .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|address| address.is_loopback())
}

fn endpoint(base_url: &Url, suffix: &str) -> Result<Url, FleetApiError> {
    base_url
        .join(suffix)
        .map_err(|_| FleetApiError::InvalidEndpoint)
}

fn fixed_proxy_endpoint(base_url: &Url, path: &str) -> Result<Url, FleetApiError> {
    let mut endpoint = base_url.clone();
    endpoint.set_path(path);
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    if endpoint.path() != path {
        return Err(FleetApiError::InvalidEndpoint);
    }
    Ok(endpoint)
}

async fn execute_json<T>(request: reqwest::RequestBuilder) -> Result<T, FleetApiError>
where
    T: DeserializeOwned,
{
    let response = request.send().await.map_err(classify_transport_error)?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        if status == 429 {
            return Err(FleetApiError::RateLimited {
                retry_after_seconds: parse_retry_after(response.headers()),
            });
        }
        // Keep existing auth recovery and plain-status matching exact. Other
        // Tesla/proxy failures may carry a useful bounded JSON error envelope.
        if !matches!(status, 401 | 403)
            && let Ok(bytes) = read_response_with_limit(response, MAX_ERROR_RESPONSE_BYTES).await
            && let Some((error, description)) = parse_provider_error(&bytes)
        {
            return Err(FleetApiError::ProviderHttpStatus {
                status,
                error,
                description,
            });
        }
        return Err(FleetApiError::HttpStatus(status));
    }
    let bytes = read_limited_response(response).await?;
    serde_json::from_slice(&bytes).map_err(|_| FleetApiError::InvalidResponse)
}

fn parse_provider_error(bytes: &[u8]) -> Option<(String, Option<String>)> {
    let envelope: ProviderErrorEnvelope = serde_json::from_slice(bytes).ok()?;
    if !safe_provider_error_code(&envelope.error) {
        return None;
    }
    // Provider descriptions are uncontrolled prose. Never retain them in an
    // error value because Debug/error-chain logging could persist credentials
    // or private vehicle data added by a future proxy/provider response.
    Some((envelope.error, None))
}

fn safe_provider_error_code(value: &str) -> bool {
    value.len() <= MAX_PROVIDER_ERROR_BYTES
        && matches!(
            value,
            "bad_request"
                | "configuration_error"
                | "forbidden"
                | "internal_error"
                | "invalid_client"
                | "invalid_config"
                | "invalid_field"
                | "invalid_grant"
                | "invalid_request"
                | "invalid_scope"
                | "invalid_token"
                | "not_found"
                | "rate_limited"
                | "service_unavailable"
                | "temporarily_unavailable"
                | "timeout"
                | "unauthorized"
                | "unsupported_grant_type"
                | "vehicle_not_found"
                | "vehicle_offline"
                | "vehicle_unavailable"
        )
}

fn classify_transport_error(error: reqwest::Error) -> FleetApiError {
    if error.is_connect() {
        FleetApiError::RequestNotSent
    } else if error.is_timeout() {
        FleetApiError::RequestTimeout
    } else {
        FleetApiError::Transport
    }
}

fn parse_retry_after(headers: &HeaderMap) -> u64 {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(300)
}

fn valid_short_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn parse_numeric_id(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text)
            if !text.is_empty()
                && text.len() <= 20
                && text.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            text.parse().ok()
        }
        _ => None,
    }
    .filter(|value| (1..=i64::MAX as u64).contains(value))
}

async fn read_limited_response(response: reqwest::Response) -> Result<Vec<u8>, FleetApiError> {
    read_response_with_limit(response, MAX_RESPONSE_BYTES).await
}

async fn read_response_with_limit(
    response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, FleetApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(FleetApiError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| FleetApiError::ResponseRead)?;
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(FleetApiError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::State,
        http::{Request, StatusCode},
        response::IntoResponse,
        routing::any,
    };
    use tokio::{net::TcpListener, task::JoinHandle};

    use super::*;

    const TEST_TOKEN: &str = "fleet-test-token";
    const TEST_VIN: &str = "5YJ3E1EA7KF000001";

    fn telemetry_ca_pem() -> String {
        let mut params = rcgen::CertificateParams::new(Vec::new()).expect("CA params");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages.push(rcgen::KeyUsagePurpose::KeyCertSign);
        params
            .self_signed(&rcgen::KeyPair::generate().expect("CA key"))
            .expect("CA certificate")
            .pem()
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct RecordedRequest {
        method: String,
        path: String,
        authorization_ok: bool,
        content_type_json: bool,
        body: Vec<u8>,
    }

    #[derive(Clone, Copy)]
    enum FakeResponse {
        Normal,
        Oversized,
        ProviderError,
        OversizedProviderError,
        UnauthorizedProviderError,
        Redirect,
        RateLimited,
        GatewayTimeout,
    }

    #[derive(Clone)]
    struct FakeState {
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        response: FakeResponse,
    }

    struct FakeServer {
        base_url: Url,
        state: FakeState,
        _task: JoinHandle<()>,
    }

    impl FakeServer {
        async fn spawn(response: FakeResponse) -> Self {
            let state = FakeState {
                requests: Arc::new(Mutex::new(Vec::new())),
                response,
            };
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("fake listener");
            let address = listener.local_addr().expect("fake address");
            let router = Router::new()
                .fallback(any(fake_handler))
                .with_state(state.clone());
            let task = tokio::spawn(async move {
                axum::serve(listener, router).await.expect("fake server");
            });
            Self {
                base_url: Url::parse(&format!("http://{address}/")).expect("fake URL"),
                state,
                _task: task,
            }
        }

        fn fleet_client(&self) -> FleetApi {
            FleetApi::for_fake_http(self.base_url.clone(), Duration::from_secs(2))
                .expect("fake Fleet client")
        }

        fn proxy_client(&self) -> FleetCommandProxy {
            let base = FleetCommandProxyBase::parse_loopback_http_for_test(self.base_url.as_str())
                .expect("fake proxy base");
            FleetCommandProxy::for_fake_http(base, Duration::from_secs(2))
                .expect("fake proxy client")
        }
    }

    async fn fake_handler(
        State(state): State<FakeState>,
        request: Request<Body>,
    ) -> impl IntoResponse {
        let method = request.method().to_string();
        let route_path = request.uri().path().to_owned();
        let path = request
            .uri()
            .path_and_query()
            .map_or_else(|| route_path.clone(), |value| value.as_str().to_owned());
        let authorization_ok = request
            .headers()
            .get("authorization")
            .is_some_and(|value| value.as_bytes() == b"Bearer fleet-test-token");
        let content_type_json = request
            .headers()
            .get("content-type")
            .is_some_and(|value| value.as_bytes() == b"application/json");
        let body = to_bytes(request.into_body(), 4096)
            .await
            .expect("bounded request body")
            .to_vec();
        state
            .requests
            .lock()
            .expect("request ledger")
            .push(RecordedRequest {
                method,
                path: path.clone(),
                authorization_ok,
                content_type_json,
                body,
            });
        match state.response {
            FakeResponse::Oversized => {
                return (StatusCode::OK, vec![b'x'; MAX_RESPONSE_BYTES + 1]).into_response();
            }
            FakeResponse::ProviderError => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!(
                        r#"{{"response":null,"error":"invalid_field","error_description":"configuration rejected","txid":"not-retained","vin":"{TEST_VIN}","token":"not-retained"}}"#
                    ),
                )
                    .into_response();
            }
            FakeResponse::OversizedProviderError => {
                return (
                    StatusCode::BAD_REQUEST,
                    vec![b'x'; MAX_ERROR_RESPONSE_BYTES + 1],
                )
                    .into_response();
            }
            FakeResponse::UnauthorizedProviderError => {
                return (
                    StatusCode::UNAUTHORIZED,
                    r#"{"error":"token rejected","error_description":"not retained"}"#,
                )
                    .into_response();
            }
            FakeResponse::Redirect => {
                return (
                    StatusCode::TEMPORARY_REDIRECT,
                    [("location", "/followed")],
                    "",
                )
                    .into_response();
            }
            FakeResponse::RateLimited => {
                return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "17")], "")
                    .into_response();
            }
            FakeResponse::GatewayTimeout => {
                return (StatusCode::GATEWAY_TIMEOUT, "").into_response();
            }
            FakeResponse::Normal => {}
        }
        if route_path == "/oauth2/v3/token" {
            (
                StatusCode::OK,
                r#"{"access_token":"fleet-next-access","refresh_token":"fleet-next-refresh","expires_in":28800,"token_type":"Bearer"}"#,
            )
                .into_response()
        } else if route_path == "/api/1/vehicles" {
            (
                StatusCode::OK,
                format!(
                    r#"{{"response":[{{"id":70,"vehicle_id":71,"vin":"{TEST_VIN}","state":"online","display_name":"Athena"}}],"count":1}}"#
                ),
            )
                .into_response()
        } else if route_path.ends_with("/vehicle_data") {
            (
                StatusCode::OK,
                r#"{"response":{"drive_state":{"timestamp":1700000000000},"charge_state":{"battery_level":80}}}"#,
            )
                .into_response()
        } else if route_path.ends_with("/wake_up") {
            (StatusCode::OK, r#"{"response":{"state":"online"}}"#).into_response()
        } else if route_path == "/api/1/vehicles/fleet_telemetry_config" {
            (
                StatusCode::OK,
                r#"{"response":{"updated_vehicles":1,"skipped_vehicles":{}}}"#,
            )
                .into_response()
        } else {
            (
                StatusCode::OK,
                r#"{"response":{"result":true,"reason":""}}"#,
            )
                .into_response()
        }
    }

    #[test]
    fn regions_are_fixed_to_official_hosts() {
        assert_eq!(
            FleetRegion::NorthAmericaAndAsiaPacific.base_url(),
            "https://fleet-api.prd.na.vn.cloud.tesla.com/"
        );
        assert_eq!(
            FleetRegion::EuropeMiddleEastAndAfrica.base_url(),
            "https://fleet-api.prd.eu.vn.cloud.tesla.com/"
        );
        assert_eq!(
            FleetRegion::China.base_url(),
            "https://fleet-api.prd.cn.vn.cloud.tesla.cn/"
        );
        assert_eq!(
            FleetRegion::EuropeMiddleEastAndAfrica.auth_token_url(),
            "https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token"
        );
        assert_eq!(
            FleetRegion::China.auth_token_url(),
            "https://auth.tesla.cn/oauth2/v3/token"
        );
        assert!(FleetApi::new(FleetRegion::EuropeMiddleEastAndAfrica, Duration::ZERO).is_err());
    }

    #[test]
    fn access_token_and_vin_are_bounded_and_redacted() {
        let token = FleetAccessToken::new(TEST_TOKEN).expect("token");
        assert_eq!(format!("{token:?}"), "FleetAccessToken([redacted])");
        assert!(!format!("{token:?}").contains(TEST_TOKEN));
        assert!(FleetAccessToken::new("bad token").is_err());
        assert!(FleetRefreshToken::new("bad token").is_err());
        assert_eq!(
            format!("{:?}", FleetClientId::parse("client-id").expect("client")),
            "FleetClientId([redacted])"
        );

        let vin = VehicleVin::parse(&TEST_VIN.to_ascii_lowercase()).expect("VIN");
        assert_eq!(vin.as_str(), TEST_VIN);
        for invalid in ["", "../wake_up", "5YJ3E1EA7KF00000I", "5YJ3E1EA7KF000001x"] {
            assert!(VehicleVin::parse(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[tokio::test]
    async fn fleet_refresh_uses_official_form_contract_without_redirects() {
        let fake = FakeServer::spawn(FakeResponse::Normal).await;
        let endpoint = fake.base_url.join("oauth2/v3/token").expect("auth URL");
        let auth = FleetAuthApi::for_fake_http(endpoint, Duration::from_secs(2))
            .expect("Fleet auth client");
        let refreshed = auth
            .refresh(
                &FleetClientId::parse("client-id").expect("client id"),
                &FleetRefreshToken::new("old-refresh").expect("refresh token"),
            )
            .await
            .expect("refreshed tokens");
        assert_eq!(refreshed.expires_in_seconds, 28_800);
        assert_eq!(refreshed.access_token.expose(), "fleet-next-access");
        assert_eq!(refreshed.refresh_token.expose(), "fleet-next-refresh");
        let requests = fake.state.requests.lock().expect("ledger");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/oauth2/v3/token");
        assert_eq!(
            requests[0].body,
            b"grant_type=refresh_token&client_id=client-id&refresh_token=old-refresh"
        );
    }

    #[test]
    fn proxy_requires_loopback_https_without_url_secrets() {
        assert!(FleetCommandProxyBase::parse("https://127.0.0.1:4443/").is_ok());
        assert!(FleetCommandProxyBase::parse("https://[::1]:4443/").is_ok());
        assert!(FleetCommandProxyBase::parse("https://localhost:4443/").is_ok());
        for invalid in [
            "http://127.0.0.1:4443/",
            "https://192.0.2.1:4443/",
            "https://token@127.0.0.1:4443/",
            "https://127.0.0.1:4443/?token=bad",
        ] {
            assert!(
                FleetCommandProxyBase::parse(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn proxy_custom_root_uses_exclusive_reqwest_trust_mode() {
        crate::crypto::install_default_provider();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
            .expect("test certificate")
            .cert
            .pem();

        command_proxy_client_builder(Duration::from_secs(2), Some(certificate.as_bytes()), false)
            .expect("custom-root builder")
            .tls_danger_accept_invalid_hostnames(true)
            .build()
            .expect("exclusive-root mode permits the reqwest-only hostname override");

        assert!(
            command_proxy_client_builder(Duration::from_secs(2), None, false)
                .expect("platform-root builder")
                .tls_danger_accept_invalid_hostnames(true)
                .build()
                .is_err(),
            "no custom root must retain reqwest's platform verifier mode"
        );
    }

    #[tokio::test]
    async fn wake_uses_exact_vin_path_and_bearer() {
        let fake = FakeServer::spawn(FakeResponse::Normal).await;
        let result = fake
            .fleet_client()
            .wake(
                &FleetAccessToken::new(TEST_TOKEN).expect("token"),
                &VehicleVin::parse(TEST_VIN).expect("VIN"),
            )
            .await
            .expect("wake");
        assert_eq!(result.state, "online");
        assert_eq!(
            *fake.state.requests.lock().expect("ledger"),
            vec![RecordedRequest {
                method: "POST".to_owned(),
                path: format!("/api/1/vehicles/{TEST_VIN}/wake_up"),
                authorization_ok: true,
                content_type_json: false,
                body: Vec::new(),
            }]
        );
    }

    #[tokio::test]
    async fn fleet_reads_discovery_and_vehicle_data_without_wake() {
        let fake = FakeServer::spawn(FakeResponse::Normal).await;
        let client = fake.fleet_client();
        let token = FleetAccessToken::new(TEST_TOKEN).expect("token");
        let vehicles = client.list_vehicles(&token).await.expect("vehicles");
        assert_eq!(vehicles.len(), 1);
        assert_eq!(vehicles[0].id.get(), 70);
        let data = client
            .vehicle_data(
                &token,
                vehicles[0].id,
                &VehicleVin::parse(TEST_VIN).expect("VIN"),
            )
            .await
            .expect("vehicle data");
        assert_eq!(data.fields()["charge_state"]["battery_level"], 80);
        let requests = fake.state.requests.lock().expect("ledger");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].path, "/api/1/vehicles");
        assert_eq!(
            requests[1].path,
            format!(
                "/api/1/vehicles/{TEST_VIN}/vehicle_data?endpoints={}",
                VEHICLE_DATA_ENDPOINTS.replace(';', "%3B")
            )
        );
        assert!(requests.iter().all(|request| request.method == "GET"));
    }

    #[tokio::test]
    async fn typed_commands_use_exact_proxy_paths_and_bodies() {
        let fake = FakeServer::spawn(FakeResponse::Normal).await;
        let proxy = fake.proxy_client();
        let token = FleetAccessToken::new(TEST_TOKEN).expect("token");
        let vin = VehicleVin::parse(TEST_VIN).expect("VIN");
        let commands = [
            (
                FleetCommand::ClimateStart,
                "auto_conditioning_start",
                b"{}".as_slice(),
            ),
            (
                FleetCommand::ClimateStop,
                "auto_conditioning_stop",
                b"{}".as_slice(),
            ),
            (FleetCommand::Lock, "door_lock", b"{}".as_slice()),
            (FleetCommand::Unlock, "door_unlock", b"{}".as_slice()),
            (FleetCommand::ChargeStart, "charge_start", b"{}".as_slice()),
            (FleetCommand::ChargeStop, "charge_stop", b"{}".as_slice()),
            (FleetCommand::FlashLights, "flash_lights", b"{}".as_slice()),
            (FleetCommand::HonkHorn, "honk_horn", b"{}".as_slice()),
            (
                FleetCommand::SetChargeLimit { percent: 80 },
                "set_charge_limit",
                br#"{"percent":80}"#.as_slice(),
            ),
        ];
        for (command, _, _) in commands {
            assert_eq!(
                proxy.execute(&token, &vin, command).await.expect("command"),
                FleetCommandResult {
                    result: true,
                    reason: None,
                }
            );
        }
        let requests = fake.state.requests.lock().expect("ledger");
        assert_eq!(requests.len(), commands.len());
        for (request, (_, endpoint, body)) in requests.iter().zip(commands) {
            assert_eq!(request.method, "POST");
            assert_eq!(
                request.path,
                format!("/api/1/vehicles/{TEST_VIN}/command/{endpoint}")
            );
            assert!(request.authorization_ok);
            assert!(request.content_type_json);
            assert_eq!(request.body, body);
        }
    }

    #[tokio::test]
    async fn fleet_telemetry_configuration_uses_fixed_proxy_path_and_typed_body() {
        let fake = FakeServer::spawn(FakeResponse::Normal).await;
        let certificate = telemetry_ca_pem();
        let config = FleetTelemetryConfigBuilder::new(
            "telemetry.example.com",
            443,
            certificate,
            1_900_000_000,
        )
        .with_recommended_fields()
        .build()
        .expect("telemetry config");
        let vins = FleetTelemetryVins::parse([TEST_VIN]).expect("VIN list");
        let result = fake
            .proxy_client()
            .configure_fleet_telemetry(
                &FleetAccessToken::new(TEST_TOKEN).expect("token"),
                &vins,
                &config,
            )
            .await
            .expect("telemetry configuration");
        assert!(result.skipped_vehicles.is_empty());

        let requests = fake.state.requests.lock().expect("ledger");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/1/vehicles/fleet_telemetry_config");
        assert!(requests[0].authorization_ok);
        assert!(requests[0].content_type_json);
        let body: Value = serde_json::from_slice(&requests[0].body).expect("JSON body");
        assert_eq!(body["vins"][0], TEST_VIN);
        assert_eq!(body["config"]["hostname"], "telemetry.example.com");
        assert_eq!(body["config"]["port"], 443);
        assert_eq!(body["config"]["exp"], 1_900_000_000u64);
        assert_eq!(body["config"]["delivery_policy"], "latest");
        assert_eq!(body["config"]["fields"]["Location"]["interval_seconds"], 5);
        assert_eq!(
            body["config"]["fields"]["Version"]["interval_seconds"],
            3600
        );
        assert_eq!(result.updated_vehicles, 1);
    }

    #[tokio::test]
    async fn fleet_telemetry_removal_uses_one_validated_vin_and_no_body() {
        let fake = FakeServer::spawn(FakeResponse::Normal).await;
        fake.proxy_client()
            .remove_fleet_telemetry(
                &FleetAccessToken::new(TEST_TOKEN).expect("token"),
                &VehicleVin::parse(TEST_VIN).expect("VIN"),
            )
            .await
            .expect("telemetry removal");

        let requests = fake.state.requests.lock().expect("ledger");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(
            requests[0].path,
            format!("/api/1/vehicles/{TEST_VIN}/fleet_telemetry_config")
        );
        assert!(requests[0].authorization_ok);
        assert!(!requests[0].content_type_json);
        assert!(requests[0].body.is_empty());
    }

    #[test]
    fn fleet_telemetry_configuration_parses_reason_grouped_skips() {
        let requested =
            FleetTelemetryVins::parse([TEST_VIN, "5YJ3E1EA7KF317001", "5YJ3E1EA7KF317002"])
                .expect("requested VINs");
        let wire: FleetTelemetryConfigureWire = serde_json::from_value(serde_json::json!({
            "updated_vehicles": 1,
            "skipped_vehicles": {
                "missing_key": ["5YJ3E1EA7KF317001"],
                "unsupported_firmware": ["5YJ3E1EA7KF317002"]
            }
        }))
        .expect("Tesla response shape");
        let result = wire.into_result(&requested).expect("validated result");
        assert_eq!(result.updated_vehicles, 1);
        assert_eq!(result.skipped_vehicles.len(), 2);
        assert_eq!(result.skipped_vehicles[0].reason, "missing_key");
        assert_eq!(result.skipped_vehicles[1].reason, "unsupported_firmware");

        let inconsistent: FleetTelemetryConfigureWire = serde_json::from_value(serde_json::json!({
            "updated_vehicles": 1,
            "skipped_vehicles": {}
        }))
        .expect("response shape");
        assert_eq!(
            inconsistent.into_result(&requested),
            Err(FleetApiError::InvalidResponse)
        );
    }

    #[test]
    fn fleet_telemetry_builder_rejects_unsafe_or_ambiguous_values() {
        let certificate = telemetry_ca_pem();
        assert_eq!(
            FleetTelemetryConfigBuilder::new(
                "https://telemetry.example.com",
                443,
                certificate.as_str(),
                1,
            )
            .with_recommended_fields()
            .build()
            .expect_err("URL is not a hostname"),
            FleetApiConfigError::InvalidTelemetryHostname
        );
        assert_eq!(
            FleetTelemetryConfigBuilder::new("telemetry.example.com", 0, certificate.as_str(), 1)
                .with_recommended_fields()
                .build()
                .expect_err("zero port"),
            FleetApiConfigError::InvalidTelemetryPort
        );
        assert_eq!(
            FleetTelemetryConfigBuilder::new("telemetry.example.com", 443, "not a certificate", 1)
                .with_recommended_fields()
                .build()
                .expect_err("invalid CA"),
            FleetApiConfigError::InvalidTelemetryCa
        );
        assert_eq!(
            FleetTelemetryConfigBuilder::new("telemetry.example.com", 443, certificate.as_str(), 1)
                .build()
                .expect_err("empty fields"),
            FleetApiConfigError::InvalidTelemetryFields
        );
        let minimum_delta = FleetTelemetryFieldConfig::new(60)
            .expect("field interval")
            .with_minimum_delta(1.0)
            .expect("minimum delta");
        assert_eq!(
            FleetTelemetryConfigBuilder::new(
                "telemetry.example.com",
                443,
                certificate.as_str(),
                1,
            )
            .field(FleetTelemetryField::VehicleSpeed, minimum_delta)
            .build()
            .expect_err("minimum delta is Location-only"),
            FleetApiConfigError::InvalidTelemetryMinimumDelta
        );
        assert_eq!(
            FleetTelemetryVins::parse([TEST_VIN, TEST_VIN]).expect_err("duplicate VIN"),
            FleetApiConfigError::DuplicateTelemetryVin
        );
    }

    #[test]
    fn fleet_telemetry_debug_redacts_ca() {
        let config = FleetTelemetryConfigBuilder::new(
            "telemetry.example.com",
            443,
            "-----BEGIN CERTIFICATE-----\nsecret-ca\n-----END CERTIFICATE-----",
            1,
        );
        assert!(!format!("{config:?}").contains("secret-ca"));
    }

    #[test]
    fn fleet_telemetry_typed_policy_matches_ingest_mapping_policy() {
        let typed = serde_json::to_value(FleetTelemetryConfigBuilder::recommended_fields())
            .expect("typed field policy JSON");
        assert_eq!(
            typed,
            crate::fleet_telemetry::recommended_cheap_fields_config()["fields"]
        );
    }

    #[tokio::test]
    async fn invalid_charge_limit_fails_before_transport() {
        let fake = FakeServer::spawn(FakeResponse::Normal).await;
        let error = fake
            .proxy_client()
            .execute(
                &FleetAccessToken::new(TEST_TOKEN).expect("token"),
                &VehicleVin::parse(TEST_VIN).expect("VIN"),
                FleetCommand::SetChargeLimit { percent: 49 },
            )
            .await
            .expect_err("invalid limit");
        assert_eq!(error, FleetApiError::InvalidChargeLimit);
        assert!(fake.state.requests.lock().expect("ledger").is_empty());
    }

    #[tokio::test]
    async fn responses_are_bounded() {
        let fake = FakeServer::spawn(FakeResponse::Oversized).await;
        let error = fake
            .fleet_client()
            .wake(
                &FleetAccessToken::new(TEST_TOKEN).expect("token"),
                &VehicleVin::parse(TEST_VIN).expect("VIN"),
            )
            .await
            .expect_err("oversized response");
        assert_eq!(error, FleetApiError::ResponseTooLarge);
    }

    #[tokio::test]
    async fn redirects_are_not_followed_and_retry_after_is_preserved() {
        let token = FleetAccessToken::new(TEST_TOKEN).expect("token");
        let vin = VehicleVin::parse(TEST_VIN).expect("VIN");

        let redirected = FakeServer::spawn(FakeResponse::Redirect).await;
        assert_eq!(
            redirected
                .fleet_client()
                .wake(&token, &vin)
                .await
                .expect_err("redirect rejected"),
            FleetApiError::HttpStatus(307)
        );
        assert_eq!(redirected.state.requests.lock().expect("ledger").len(), 1);

        let limited = FakeServer::spawn(FakeResponse::RateLimited).await;
        assert_eq!(
            limited
                .fleet_client()
                .wake(&token, &vin)
                .await
                .expect_err("rate limit surfaced"),
            FleetApiError::RateLimited {
                retry_after_seconds: 17,
            }
        );

        let timed_out = FakeServer::spawn(FakeResponse::GatewayTimeout).await;
        assert_eq!(
            timed_out
                .fleet_client()
                .wake(&token, &vin)
                .await
                .expect_err("explicit timeout status surfaced"),
            FleetApiError::HttpStatus(504)
        );
    }

    #[tokio::test]
    async fn provider_errors_are_bounded_allowlisted_and_keep_auth_status_plain() {
        let token = FleetAccessToken::new(TEST_TOKEN).expect("token");
        let vin = VehicleVin::parse(TEST_VIN).expect("VIN");

        let provider = FakeServer::spawn(FakeResponse::ProviderError).await;
        let error = provider
            .fleet_client()
            .wake(&token, &vin)
            .await
            .expect_err("provider error surfaced");
        assert_eq!(error.http_status(), Some(400));
        assert_eq!(error.provider_error(), Some(("invalid_field", None)));
        let retained = format!("{error:?}");
        assert!(!retained.contains(TEST_VIN));
        assert!(!retained.contains(TEST_TOKEN));
        assert!(!retained.contains("not-retained"));

        let oversized = FakeServer::spawn(FakeResponse::OversizedProviderError).await;
        assert_eq!(
            oversized
                .fleet_client()
                .wake(&token, &vin)
                .await
                .expect_err("oversized error body rejected"),
            FleetApiError::HttpStatus(400)
        );

        let unauthorized = FakeServer::spawn(FakeResponse::UnauthorizedProviderError).await;
        assert_eq!(
            unauthorized
                .fleet_client()
                .wake(&token, &vin)
                .await
                .expect_err("auth status remains matchable"),
            FleetApiError::HttpStatus(401)
        );
    }

    #[test]
    fn sensitive_or_non_printable_provider_error_text_is_not_retained() {
        for body in [
            format!(r#"{{"error":"vehicle {TEST_VIN} not_found"}}"#),
            r#"{"error":"Bearer exposed-value"}"#.to_owned(),
            r#"{"error":"-----BEGIN CERTIFICATE-----"}"#.to_owned(),
            r#"{"error":"bad\nfield"}"#.to_owned(),
            r#"{"error":"opaque_secret_material_1234567890"}"#.to_owned(),
        ] {
            assert_eq!(parse_provider_error(body.as_bytes()), None);
        }
        assert_eq!(
            parse_provider_error(
                br#"{"error":"vehicle_offline","error_description":"do-not-retain"}"#
            ),
            Some(("vehicle_offline".to_owned(), None))
        );
    }

    #[tokio::test]
    async fn connection_failure_is_known_unsent() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve local port");
        let address = listener.local_addr().expect("local address");
        drop(listener);
        let endpoint =
            Url::parse(&format!("http://{address}/oauth2/v3/token")).expect("fake auth URL");
        let auth = FleetAuthApi::for_fake_http(endpoint, Duration::from_millis(250))
            .expect("Fleet auth client");

        assert_eq!(
            auth.refresh(
                &FleetClientId::parse("client-id").expect("client"),
                &FleetRefreshToken::new("old-refresh").expect("refresh token"),
            )
            .await
            .expect_err("connection must fail"),
            FleetApiError::RequestNotSent
        );
    }
}
