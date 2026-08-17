//! Deliberate, one-shot compatibility reads from a Tesla Owner API endpoint.
//!
//! This is not a polling loop, a Fleet implementation, or a command client.
//! It only sends authenticated `GET` requests to the legacy-compatible product list and
//! crate-local `vehicle_data` paths. The collector owns the no-wake stream-power
//! confirmation contract; this module exposes no public manual collection shortcut.
//!
//! Legacy authentication is supplied only through the crate-local
//! `LegacyAuthManager`; raw bearer strings never enter the production API.

use std::{
    fmt,
    time::{Duration, SystemTime},
};

use futures_util::StreamExt;
use reqwest::{
    Client,
    header::{ACCEPT, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use thiserror::Error;
use url::Url;

use crate::{
    credentials::{LegacyAuthManager, LegacyAuthManagerError},
    hub_pack::ProjectionCarSettings,
    legacy_auth::LegacyAuthFuse,
    tesla_stream::{StreamPowerGate, StreamRegion},
};

/// Four MiB is comfortably above a normal vehicle-data response while keeping
/// a bad upstream response from turning a manual collection into an unbounded
/// allocation.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const LEGACY_AUTH_TIMEOUT: Duration = Duration::from_secs(60);
const ACCEPT_JSON: HeaderValue = HeaderValue::from_static("application/json");
const VEHICLE_DATA_ENDPOINTS: &str = "charge_state;climate_state;closures_state;drive_state;gui_settings;location_data;vehicle_config;vehicle_state;vehicle_data_combo";

/// A validated, explicit HTTPS Owner API base URL.
///
/// The default configuration uses Tesla's legacy Owner API. Loopback HTTP is
/// accepted only for local fake-Tesla tests.
#[derive(Clone, PartialEq, Eq)]
pub struct OwnerApiBase {
    url: Url,
}

impl OwnerApiBase {
    pub fn parse(value: &str) -> Result<Self, OwnerApiConfigError> {
        let url = Url::parse(value).map_err(|_| OwnerApiConfigError::InvalidBaseUrl)?;
        Self::from_url(url, true)
    }

    fn from_url(mut url: Url, require_https: bool) -> Result<Self, OwnerApiConfigError> {
        if require_https && url.scheme() != "https" {
            // Local replacement proof uses a loopback fake Tesla source over
            // plaintext HTTP. Remote hosts remain HTTPS-only.
            if url.scheme() != "http" || !is_loopback_owner_api_host(url.host_str()) {
                return Err(OwnerApiConfigError::HttpsRequired);
            }
        }
        if !require_https && !matches!(url.scheme(), "http" | "https") {
            return Err(OwnerApiConfigError::UnsupportedBaseScheme);
        }
        if url.cannot_be_a_base() || url.host_str().is_none() {
            return Err(OwnerApiConfigError::BaseHostRequired);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(OwnerApiConfigError::EmbeddedBaseCredential);
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(OwnerApiConfigError::BaseParametersNotPermitted);
        }
        if url
            .path_segments()
            .is_some_and(|mut segments| segments.any(|segment| segment == ".."))
        {
            return Err(OwnerApiConfigError::BasePathTraversal);
        }
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }

        Ok(Self { url })
    }

    /// True when this base is loopback HTTP (local fake Tesla only).
    pub fn is_loopback_http(&self) -> bool {
        self.url.scheme() == "http" && is_loopback_owner_api_host(self.url.host_str())
    }

    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    fn endpoint(&self, suffix: &str) -> Result<Url, OwnerApiError> {
        self.url
            .join(suffix)
            .map_err(|_| OwnerApiError::InvalidEndpoint)
    }

    pub fn stream_region(&self) -> Option<StreamRegion> {
        let host = self.url.host_str()?.to_ascii_lowercase();
        if host == "auth.tesla.cn"
            || host.ends_with(".tesla.cn")
            || host.ends_with(".cloud.tesla.cn")
        {
            Some(StreamRegion::China)
        } else if host == "auth.tesla.com"
            || host.ends_with(".tesla.com")
            || host.ends_with(".teslamotors.com")
        {
            Some(StreamRegion::Global)
        } else {
            None
        }
    }
}

impl fmt::Debug for OwnerApiBase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("OwnerApiBase")
            .field(&self.url.as_str())
            .finish()
    }
}

/// Construction-only settings for a manually invoked compatibility read.
#[derive(Clone, Debug)]
pub struct OwnerApiOptions {
    pub base_url: OwnerApiBase,
    pub request_timeout: Duration,
}

impl OwnerApiOptions {
    pub fn new(base_url: OwnerApiBase, request_timeout: Duration) -> Self {
        Self {
            base_url,
            request_timeout,
        }
    }
}

/// A narrowly scoped, read-only Owner API client.
#[derive(Clone)]
pub(crate) struct OwnerApi {
    client: Client,
    legacy_auth_client: Client,
    base_url: OwnerApiBase,
}

fn is_loopback_owner_api_host(host: Option<&str>) -> bool {
    matches!(host, Some("127.0.0.1" | "localhost" | "::1"))
}

impl OwnerApi {
    pub(crate) fn new(options: OwnerApiOptions) -> Result<Self, OwnerApiConfigError> {
        // Loopback HTTP is only accepted for the local fake Tesla source used by
        // replacement journeys. Remote endpoints still require HTTPS.
        let allow_loopback_http = options.base_url.is_loopback_http();
        Self::build(options, allow_loopback_http)
    }

    pub(crate) fn legacy_auth_http_client(&self) -> Client {
        self.legacy_auth_client.clone()
    }

    fn build(
        options: OwnerApiOptions,
        allow_insecure_test_base: bool,
    ) -> Result<Self, OwnerApiConfigError> {
        if options.request_timeout.is_zero() {
            return Err(OwnerApiConfigError::ZeroTimeout);
        }

        // Owner API construction is also used by standalone collection tests
        // and commands, so install the one Hub TLS provider at this boundary
        // instead of relying on the serving or PostgreSQL path to run first.
        crate::crypto::install_default_provider();
        let client = Client::builder()
            .https_only(!allow_insecure_test_base)
            .redirect(Policy::none())
            .timeout(options.request_timeout)
            .build()
            .map_err(|_| OwnerApiConfigError::ClientBuild)?;
        let legacy_auth_client = Client::builder()
            .https_only(!allow_insecure_test_base)
            // A refresh response is credential-bearing. Never replay the
            // refresh body or headers after a redirect, even same-origin.
            // The admission guard covers the original request only; following
            // a redirect would create an unguarded second transport.
            .redirect(Policy::none())
            .timeout(LEGACY_AUTH_TIMEOUT)
            .read_timeout(LEGACY_AUTH_TIMEOUT)
            .build()
            .map_err(|_| OwnerApiConfigError::ClientBuild)?;

        Ok(Self {
            client,
            legacy_auth_client,
            base_url: options.base_url,
        })
    }

    /// Legacy owner-authenticated discovery performs one Owner request. The
    /// collector owns any asynchronous recovery after a wrapped 401.
    #[cfg(test)]
    pub(crate) async fn list_vehicles_with_legacy_auth(
        &self,
        auth: &mut LegacyAuthManager,
    ) -> Result<Vec<Vehicle>, OwnerApiAuthError> {
        let mut fuse = LegacyAuthFuse::default();
        self.list_vehicles_with_legacy_auth_fused(auth, &mut fuse)
            .await
    }

    pub(crate) async fn list_vehicles_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
    ) -> Result<Vec<Vehicle>, OwnerApiAuthError> {
        let endpoint = self
            .base_url
            .endpoint("api/1/products")
            .map_err(OwnerApiAuthError::Owner)?;
        let envelope: ResponseEnvelope<Vec<ProductWire>> = self
            .get_envelope_with_legacy_auth_url_fused(auth, fuse, endpoint, None)
            .await?;
        parse_vehicle_list(envelope).map_err(OwnerApiAuthError::Owner)
    }

    pub(crate) async fn vehicle_data_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
        power_gate: Option<&StreamPowerGate>,
    ) -> Result<VehicleData, OwnerApiAuthError> {
        let endpoint = self.vehicle_data_endpoint(vehicle_id)?;
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_with_legacy_auth_url_fused(auth, fuse, endpoint, power_gate)
            .await?;
        parse_vehicle_data(vehicle_id, envelope).map_err(OwnerApiAuthError::Owner)
    }

    pub(crate) async fn vehicle_probe_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
    ) -> Result<bool, OwnerApiAuthError> {
        let endpoint = self.vehicle_probe_endpoint(vehicle_id)?;
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_with_legacy_auth_url_fused(auth, fuse, endpoint, None)
            .await?;
        Ok(envelope
            .response
            .get("in_service")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    pub(crate) async fn vehicle_state_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
    ) -> Result<String, OwnerApiAuthError> {
        let endpoint = self.vehicle_probe_endpoint(vehicle_id)?;
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_with_legacy_auth_url_fused(auth, fuse, endpoint, None)
            .await?;
        parse_vehicle_state(envelope.response).map_err(OwnerApiAuthError::Owner)
    }

    fn vehicle_data_endpoint(&self, vehicle_id: VehicleId) -> Result<Url, OwnerApiError> {
        let suffix = format!("api/1/vehicles/{vehicle_id}/vehicle_data");
        let mut endpoint = self.base_url.endpoint(&suffix)?;
        endpoint
            .query_pairs_mut()
            .append_pair("endpoints", VEHICLE_DATA_ENDPOINTS);
        Ok(endpoint)
    }

    fn vehicle_probe_endpoint(&self, vehicle_id: VehicleId) -> Result<Url, OwnerApiError> {
        self.base_url
            .endpoint(&format!("api/1/vehicles/{vehicle_id}"))
    }

    async fn get_envelope_with_legacy_auth_url_fused<T>(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        endpoint: Url,
        power_gate: Option<&StreamPowerGate>,
    ) -> Result<T, OwnerApiAuthError>
    where
        T: DeserializeOwned,
    {
        if fuse.is_blown() {
            return Err(OwnerApiAuthError::NotSignedIn);
        }
        auth.assert_sensitive_access()?;
        // The API timer in pinned TeslaMate refreshes independently of an
        // Owner API 401. Check the same persisted schedule before each bounded
        // compatibility read; refresh failure retains the current pair.
        if let Err(error) = auth
            .refresh_if_due(&self.legacy_auth_client, SystemTime::now())
            .await
        {
            if error.is_sensitive_access_failure() {
                return Err(error.into());
            }
            if !matches!(
                &error,
                LegacyAuthManagerError::Auth(crate::legacy_auth::LegacyAuthError::RefreshDeferred)
            ) {
                tracing::warn!(error = %error, "scheduled legacy token refresh failed; current pair retained");
            }
        }
        let first = self
            .get_envelope_url_with_legacy_auth(auth, endpoint, power_gate)
            .await;
        if !matches!(
            first,
            Err(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401)))
        ) {
            return first;
        }
        fuse.record_unauthorized(SystemTime::now());
        if fuse.is_blown() {
            return Err(OwnerApiAuthError::NotSignedIn);
        }
        first
    }

    async fn get_envelope_url_with_legacy_auth<T>(
        &self,
        auth: &LegacyAuthManager,
        url: Url,
        power_gate: Option<&StreamPowerGate>,
    ) -> Result<T, OwnerApiAuthError>
    where
        T: DeserializeOwned,
    {
        if power_gate.is_some_and(|gate| !gate.is_confirmed()) {
            return Err(OwnerApiAuthError::Owner(
                OwnerApiError::StreamPowerNotConfirmed,
            ));
        }
        let request = self.envelope_request(auth.access_token_for_sensitive_use()?, url);
        auth.assert_sensitive_access()?;
        self.execute_envelope_request(request)
            .await
            .map_err(OwnerApiAuthError::Owner)
    }

    fn envelope_request(&self, bearer: &str, url: Url) -> reqwest::RequestBuilder {
        self.client
            .get(url)
            .header(ACCEPT, ACCEPT_JSON.clone())
            .bearer_auth(bearer)
    }

    async fn execute_envelope_request<T>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, OwnerApiError>
    where
        T: DeserializeOwned,
    {
        let response = request.send().await.map_err(classify_transport_error)?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            if status == 408 || status == 504 {
                return Err(OwnerApiError::RequestTimeout);
            }
            if status == 429 {
                let retry_after_seconds = parse_retry_after(response.headers());
                return Err(OwnerApiError::RateLimited {
                    retry_after_seconds,
                });
            }
            if matches!(status, 403..=405) {
                let bytes = read_limited_response(response).await?;
                if status == 405 && is_vehicle_in_service_body(&bytes) {
                    return Err(OwnerApiError::VehicleInService);
                }
                if status == 404 && is_owner_error_body(&bytes, "not_found") {
                    return Err(OwnerApiError::VehicleNotFound);
                }
                if status == 403 && is_owner_error_body(&bytes, "account disabled: EXCEEDED_LIMIT")
                {
                    return Err(OwnerApiError::RateLimited {
                        retry_after_seconds: 900,
                    });
                }
            }
            return Err(OwnerApiError::HttpStatus(status));
        }

        let bytes = read_limited_response(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| OwnerApiError::InvalidResponseEnvelope)
    }
}

fn is_owner_error_body(bytes: &[u8], expected: &str) -> bool {
    if bytes == expected.as_bytes() {
        return true;
    }
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| match value {
            Value::String(value) => Some(value),
            Value::Object(mut object) => object
                .remove("error")
                .and_then(|value| value.as_str().map(str::to_owned)),
            _ => None,
        })
        .is_some_and(|value| value == expected)
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> u64 {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(300)
}

fn parse_vehicle_list(
    envelope: ResponseEnvelope<Vec<ProductWire>>,
) -> Result<Vec<Vehicle>, OwnerApiError> {
    if let Some(count) = envelope.count
        && count != envelope.response.len()
    {
        return Err(OwnerApiError::InvalidVehicleListCount);
    }
    envelope
        .response
        .into_iter()
        .filter_map(ProductWire::into_vehicle)
        .collect::<Result<Vec<_>, _>>()
}

fn parse_vehicle_data(
    vehicle_id: VehicleId,
    envelope: ResponseEnvelope<Map<String, Value>>,
) -> Result<VehicleData, OwnerApiError> {
    if envelope.count.is_some() || envelope.response.is_empty() {
        return Err(OwnerApiError::InvalidVehicleDataEnvelope);
    }
    let mut fields = envelope.response;
    scrub_sensitive_fields(&mut fields);
    if fields.is_empty() {
        return Err(OwnerApiError::SensitiveDataInResponse);
    }
    Ok(VehicleData { vehicle_id, fields })
}

impl fmt::Debug for OwnerApi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnerApi")
            .field("base_url", &self.base_url)
            .field("redirects", &"disabled")
            .finish_non_exhaustive()
    }
}

/// Owner API vehicle identifiers are restricted to unsigned decimal values
/// before they become a path segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VehicleId(u64);

impl VehicleId {
    pub fn get(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn from_test(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for VehicleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Numeric `vehicle_id` used only as the Tesla streaming subscription tag.
/// This is distinct from the products `id`/EID used by Owner API REST paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamVehicleId(u64);

impl StreamVehicleId {
    pub fn get(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn from_test(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for StreamVehicleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Minimal, non-secret vehicle discovery data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vehicle {
    pub id: VehicleId,
    pub stream_id: StreamVehicleId,
    pub vin: String,
    pub state: String,
    pub display_name: Option<String>,
    pub settings: ProjectionCarSettings,
}

impl Vehicle {
    pub fn is_online(&self) -> bool {
        self.state == "online"
    }

    #[cfg(test)]
    pub(crate) fn for_test(id: u64, vin: &str, state: &str) -> Self {
        Self {
            id: VehicleId(id),
            stream_id: StreamVehicleId(id),
            vin: vin.to_owned(),
            state: state.to_owned(),
            display_name: None,
            settings: ProjectionCarSettings::default(),
        }
    }
}

/// A successful vehicle-data response. The raw fields are intentionally kept
/// separate from the collector's future normalizer and never appear in errors.
#[derive(Clone, PartialEq)]
pub struct VehicleData {
    vehicle_id: VehicleId,
    fields: Map<String, Value>,
}

impl VehicleData {
    pub fn vehicle_id(&self) -> VehicleId {
        self.vehicle_id
    }

    pub fn fields(&self) -> &Map<String, Value> {
        &self.fields
    }

    #[cfg(test)]
    pub(crate) fn for_test(vehicle_id: u64, fields: Value) -> Self {
        let fields = fields
            .as_object()
            .expect("test vehicle data must be an object")
            .clone();
        Self {
            vehicle_id: VehicleId(vehicle_id),
            fields,
        }
    }
}

impl fmt::Debug for VehicleData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VehicleData")
            .field("vehicle_id", &self.vehicle_id)
            .field("field_count", &self.fields.len())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ManualCollection {
    pub vehicles: Vec<Vehicle>,
    pub snapshots: Vec<VehicleData>,
    pub failures: Vec<VehicleCollectionFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VehicleCollectionFailure {
    pub vehicle_id: VehicleId,
    pub error: OwnerApiError,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OwnerApiConfigError {
    #[error("owner API base URL is invalid")]
    InvalidBaseUrl,
    #[error("owner API base URL must use HTTPS")]
    HttpsRequired,
    #[error("owner API test base URL must use HTTP or HTTPS")]
    UnsupportedBaseScheme,
    #[error("owner API base URL requires a host")]
    BaseHostRequired,
    #[error("owner API base URL cannot contain credentials")]
    EmbeddedBaseCredential,
    #[error("owner API base URL cannot contain query parameters or a fragment")]
    BaseParametersNotPermitted,
    #[error("owner API base URL cannot contain path traversal")]
    BasePathTraversal,
    #[error("owner API request timeout must be greater than zero")]
    ZeroTimeout,
    #[error("owner API HTTP client could not be constructed")]
    ClientBuild,
}

/// Every error is deliberately content-free. In particular it carries neither
/// the bearer token, a response body, nor a request URL that could contain a
/// mistakenly configured secret.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OwnerApiError {
    #[error("owner API endpoint is invalid")]
    InvalidEndpoint,
    #[error("owner API request timed out")]
    RequestTimeout,
    #[error("owner API transport failed")]
    Transport,
    #[error("owner API returned HTTP {0}")]
    HttpStatus(u16),
    #[error("owner API rate limited; retry after {retry_after_seconds}s")]
    RateLimited { retry_after_seconds: u64 },
    #[error("owner API vehicle was not found")]
    VehicleNotFound,
    #[error("owner API vehicle is in service")]
    VehicleInService,
    #[error("owner API response exceeds the size limit")]
    ResponseTooLarge,
    #[error("owner API response body could not be read")]
    ResponseRead,
    #[error("owner API response envelope is invalid")]
    InvalidResponseEnvelope,
    #[error("owner API vehicle list count is inconsistent")]
    InvalidVehicleListCount,
    #[error("owner API vehicle record is invalid")]
    InvalidVehicleRecord,
    #[error("owner API vehicle-data envelope is invalid")]
    InvalidVehicleDataEnvelope,
    #[error("owner API response contains a credential-shaped field")]
    SensitiveDataInResponse,
    #[error("owner API conditional read lost its live stream-power prerequisite")]
    StreamPowerNotConfirmed,
    #[error("owner API credential authority is unavailable")]
    CredentialAuthorityUnavailable,
    #[error("legacy owner authentication failed")]
    LegacyAuth,
}

#[derive(Debug, Error)]
pub enum OwnerApiAuthError {
    #[error("owner API request failed: {0}")]
    Owner(#[from] OwnerApiError),
    #[error("legacy auth failed: {0}")]
    Auth(#[from] LegacyAuthManagerError),
    #[error("not signed in")]
    NotSignedIn,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseEnvelope<T> {
    response: T,
    #[serde(default)]
    count: Option<usize>,
}

#[derive(Deserialize)]
struct ProductWire {
    #[serde(default)]
    vehicle_id: Option<Value>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    vin: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

impl ProductWire {
    /// `/products` can include energy products. The documented legacy
    /// vehicle discriminator is the presence of `vehicle_id`; non-vehicle
    /// products are ignored before any vehicle-data request is made.
    fn into_vehicle(self) -> Option<Result<Vehicle, OwnerApiError>> {
        let Self {
            vehicle_id,
            id,
            vin,
            state,
            display_name,
        } = self;

        let stream_id = match vehicle_id.as_ref() {
            Some(value) => match parse_stream_vehicle_id(value) {
                Ok(id) => id,
                Err(error) => return Some(Err(error)),
            },
            // Energy products have no vehicle-shaped identity fields and stay
            // outside collection. A vehicle-like record without its stream
            // identity is malformed rather than silently subscribing by EID.
            None if id.is_some() || vin.is_some() || state.is_some() => {
                return Some(Err(OwnerApiError::InvalidVehicleRecord));
            }
            None => return None,
        };

        let id = match id.as_ref().map(parse_vehicle_id) {
            Some(Ok(id)) => id,
            Some(Err(error)) => return Some(Err(error)),
            None => return Some(Err(OwnerApiError::InvalidVehicleRecord)),
        };
        let vin = match vin {
            Some(vin) => vin,
            None => return Some(Err(OwnerApiError::InvalidVehicleRecord)),
        };
        let state = match state {
            Some(state) => state,
            None => return Some(Err(OwnerApiError::InvalidVehicleRecord)),
        };
        if !valid_vin(&vin)
            || !valid_state(&state)
            || display_name
                .as_deref()
                .is_some_and(|name| name.len() > 1024)
        {
            return Some(Err(OwnerApiError::InvalidVehicleRecord));
        }

        Some(Ok(Vehicle {
            id,
            stream_id,
            vin,
            state,
            display_name,
            settings: ProjectionCarSettings::default(),
        }))
    }
}

fn parse_vehicle_id(value: &Value) -> Result<VehicleId, OwnerApiError> {
    let parsed = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text)
            if !text.is_empty()
                && text.len() <= 20
                && text.as_bytes().iter().all(u8::is_ascii_digit) =>
        {
            text.parse().ok()
        }
        _ => None,
    };
    parsed
        .filter(|id| (1..=i64::MAX as u64).contains(id))
        .map(VehicleId)
        .ok_or(OwnerApiError::InvalidVehicleRecord)
}

fn parse_stream_vehicle_id(value: &Value) -> Result<StreamVehicleId, OwnerApiError> {
    parse_numeric_vehicle_id(value)
        .map(StreamVehicleId)
        .ok_or(OwnerApiError::InvalidVehicleRecord)
}

fn parse_numeric_vehicle_id(value: &Value) -> Option<u64> {
    let parsed = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text)
            if !text.is_empty()
                && text.len() <= 20
                && text.as_bytes().iter().all(u8::is_ascii_digit) =>
        {
            text.parse().ok()
        }
        _ => None,
    };
    parsed.filter(|id| (1..=i64::MAX as u64).contains(id))
}

fn valid_vin(value: &str) -> bool {
    value.len() == 17
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && !value
            .bytes()
            .any(|byte| matches!(byte, b'I' | b'O' | b'Q' | b'i' | b'o' | b'q'))
}

fn valid_state(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn parse_vehicle_state(fields: Map<String, Value>) -> Result<String, OwnerApiError> {
    fields
        .get("state")
        .and_then(Value::as_str)
        .filter(|state| valid_state(state))
        .map(str::to_owned)
        .ok_or(OwnerApiError::InvalidVehicleRecord)
}

fn scrub_sensitive_fields(fields: &mut Map<String, Value>) {
    fields.retain(|key, value| {
        let sensitive = matches!(
            key.to_ascii_lowercase().as_str(),
            "access_token"
                | "refresh_token"
                | "authorization"
                | "token"
                | "tokens"
                | "backseat_token"
        );
        if !sensitive {
            scrub_sensitive_value(value);
        }
        !sensitive
    });
}

fn scrub_sensitive_value(value: &mut Value) {
    match value {
        Value::Object(fields) => scrub_sensitive_fields(fields),
        Value::Array(values) => values.iter_mut().for_each(scrub_sensitive_value),
        _ => {}
    }
}

fn classify_transport_error(error: reqwest::Error) -> OwnerApiError {
    if error.is_timeout() {
        OwnerApiError::RequestTimeout
    } else {
        OwnerApiError::Transport
    }
}

fn is_vehicle_in_service_body(bytes: &[u8]) -> bool {
    let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    fields.len() == 1
        && fields.get("error").and_then(Value::as_str) == Some("vehicle is currently in service")
}

async fn read_limited_response(response: reqwest::Response) -> Result<Vec<u8>, OwnerApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(OwnerApiError::ResponseTooLarge);
    }

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| OwnerApiError::ResponseRead)?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(OwnerApiError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
impl OwnerApi {
    pub(crate) fn for_fake_http(
        base_url: Url,
        request_timeout: Duration,
    ) -> Result<Self, OwnerApiConfigError> {
        let base_url = OwnerApiBase::from_url(base_url, false)?;
        Self::build(OwnerApiOptions::new(base_url, request_timeout), true)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use axum::{
        Router,
        extract::{Path as AxumPath, State},
        http::{HeaderMap, StatusCode, Uri},
        response::IntoResponse,
        routing::get,
    };
    use tokio::{net::TcpListener, task::JoinHandle};

    use super::*;
    use crate::credentials::{CredentialError, LegacyAuthManager};

    const TEST_TOKEN: &str = "test-owner-token";
    const TEST_VIN: &str = "5YJ3E1EA7KF000001";

    #[derive(Clone, Default)]
    struct FakeState {
        requests: Arc<Mutex<Vec<FakeRequest>>>,
        vehicles_body: Arc<Mutex<String>>,
        data_bodies: Arc<Mutex<BTreeMap<String, (StatusCode, String)>>>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeRequest {
        method: String,
        path: String,
        query: String,
        authorization_is_expected: bool,
    }

    impl FakeState {
        fn with_vehicles(body: &str) -> Self {
            Self {
                vehicles_body: Arc::new(Mutex::new(body.to_owned())),
                ..Self::default()
            }
        }
    }

    #[test]
    fn vehicle_products_require_a_valid_distinct_stream_id() {
        let product = |vehicle_id| ProductWire {
            vehicle_id,
            id: Some(serde_json::json!(9)),
            vin: Some(TEST_VIN.to_owned()),
            state: Some("online".to_owned()),
            display_name: None,
        };

        let vehicle = product(Some(serde_json::json!(42)))
            .into_vehicle()
            .expect("vehicle-shaped product")
            .expect("valid product");
        assert_eq!(vehicle.id.get(), 9);
        assert_eq!(vehicle.stream_id.get(), 42);

        let missing = product(None)
            .into_vehicle()
            .expect("vehicle-like product cannot be ignored")
            .expect_err("missing stream id must fail closed");
        assert_eq!(missing, OwnerApiError::InvalidVehicleRecord);

        for invalid in [
            serde_json::Value::Null,
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(""),
            serde_json::json!("not-a-number"),
            serde_json::json!(9_223_372_036_854_775_808_u64),
        ] {
            let error = product(Some(invalid))
                .into_vehicle()
                .expect("vehicle-shaped product")
                .expect_err("invalid stream id must fail closed");
            assert_eq!(error, OwnerApiError::InvalidVehicleRecord);
        }

        let energy_product = ProductWire {
            vehicle_id: None,
            id: None,
            vin: None,
            state: None,
            display_name: None,
        };
        assert!(energy_product.into_vehicle().is_none());
    }

    #[test]
    fn vehicle_data_endpoint_preserves_provider_path_and_encodes_only_the_endpoint_query() {
        let base = Url::parse("http://provider.example/owner-proxy/").expect("base URL");
        let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).expect("fake client");
        let endpoint = client
            .vehicle_data_endpoint(VehicleId(7))
            .expect("vehicle endpoint");

        assert_eq!(
            endpoint.path(),
            "/owner-proxy/api/1/vehicles/7/vehicle_data"
        );
        assert_eq!(
            endpoint.query(),
            Some(
                "endpoints=charge_state%3Bclimate_state%3Bclosures_state%3Bdrive_state%3Bgui_settings%3Blocation_data%3Bvehicle_config%3Bvehicle_state%3Bvehicle_data_combo"
            )
        );
        assert!(endpoint.username().is_empty());
        assert!(endpoint.password().is_none());
        assert!(!endpoint.query().unwrap().contains(TEST_TOKEN));
    }

    #[test]
    fn production_base_requires_explicit_https_and_rejects_secret_bearing_forms() {
        assert!(matches!(
            OwnerApiBase::parse("http://owner.example"),
            Err(OwnerApiConfigError::HttpsRequired)
        ));
        assert!(matches!(
            OwnerApiBase::parse("https://token@owner.example"),
            Err(OwnerApiConfigError::EmbeddedBaseCredential)
        ));
        assert!(matches!(
            OwnerApiBase::parse("https://owner.example/?token=bad"),
            Err(OwnerApiConfigError::BaseParametersNotPermitted)
        ));
        let base = OwnerApiBase::parse("https://owner.example/api").expect("https base");
        assert_eq!(base.url.as_str(), "https://owner.example/api/");
        assert!(matches!(
            OwnerApi::new(OwnerApiOptions::new(base, Duration::ZERO)),
            Err(OwnerApiConfigError::ZeroTimeout)
        ));
    }

    #[test]
    fn loopback_http_owner_api_base_is_accepted_for_local_fake_source() {
        let base = OwnerApiBase::parse("http://127.0.0.1:9/").expect("loopback http");
        assert!(base.is_loopback_http());
        assert_eq!(base.as_str(), "http://127.0.0.1:9/");
        let localhost = OwnerApiBase::parse("http://localhost:9/owner/").expect("localhost http");
        assert!(localhost.is_loopback_http());
        assert!(matches!(
            OwnerApiBase::parse("http://192.168.1.2/"),
            Err(OwnerApiConfigError::HttpsRequired)
        ));
    }

    struct FakeServer {
        base_url: Url,
        _task: JoinHandle<()>,
    }

    impl FakeServer {
        async fn spawn(state: FakeState) -> Self {
            Self::start(
                Router::new()
                    .route("/api/1/products", get(list_handler))
                    .route(
                        "/api/1/vehicles/{vehicle_id}/vehicle_data",
                        get(data_handler),
                    )
                    .with_state(state),
            )
            .await
        }

        async fn start(router: Router) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("fake listener");
            let address = listener.local_addr().expect("fake address");
            let task = tokio::spawn(async move {
                axum::serve(listener, router)
                    .await
                    .expect("fake server runs");
            });
            Self {
                base_url: Url::parse(&format!("http://{address}/")).expect("fake URL"),
                _task: task,
            }
        }

        fn client(&self, timeout: Duration) -> OwnerApi {
            OwnerApi::for_fake_http(self.base_url.clone(), timeout).expect("fake client")
        }
    }

    fn guarded_legacy_manager(issuer: Url, admitted: Arc<AtomicBool>) -> LegacyAuthManager {
        let auth =
            crate::legacy_auth::LegacyAuth::for_test(issuer, TEST_TOKEN, "test-refresh-token")
                .with_test_schedule(2_000_000_000, 1_900_000_000);
        LegacyAuthManager::for_test_with_sensitive_access(
            auth,
            Arc::new(|_, _| Ok(())),
            Arc::new(move || {
                admitted
                    .load(Ordering::Acquire)
                    .then_some(())
                    .ok_or(CredentialError::MacKeychainHelperInvalid)
            }),
        )
    }

    #[tokio::test]
    async fn missing_or_stale_sensitive_admission_blocks_before_owner_transport() {
        let state = FakeState::with_vehicles(r#"{"response":[],"count":0}"#);
        let fake = FakeServer::spawn(state.clone()).await;
        let admitted = Arc::new(AtomicBool::new(false));
        let mut manager = guarded_legacy_manager(fake.base_url.clone(), Arc::clone(&admitted));
        let mut fuse = LegacyAuthFuse::default();

        let error = fake
            .client(Duration::from_secs(2))
            .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
            .await
            .expect_err("missing admission must fence transport");
        assert!(matches!(
            error,
            OwnerApiAuthError::Auth(LegacyAuthManagerError::Credential(
                CredentialError::MacKeychainHelperInvalid
            ))
        ));
        assert!(state.requests.lock().expect("requests").is_empty());

        admitted.store(true, Ordering::Release);
        fake.client(Duration::from_secs(2))
            .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
            .await
            .expect("live admission permits request");
        assert_eq!(state.requests.lock().expect("requests").len(), 1);

        admitted.store(false, Ordering::Release);
        fake.client(Duration::from_secs(2))
            .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
            .await
            .expect_err("stale admission must fence transport");
        assert_eq!(state.requests.lock().expect("requests").len(), 1);
    }
    async fn list_handler(State(state): State<FakeState>, headers: HeaderMap) -> impl IntoResponse {
        record(&state, &headers, "/api/1/products");
        state.vehicles_body.lock().expect("fake list lock").clone()
    }

    async fn data_handler(
        State(state): State<FakeState>,
        AxumPath(vehicle_id): AxumPath<String>,
        headers: HeaderMap,
        uri: Uri,
    ) -> impl IntoResponse {
        let query = uri.query().unwrap_or_default();
        record_with_query(
            &state,
            &headers,
            &format!("/api/1/vehicles/{vehicle_id}/vehicle_data"),
            query,
        );
        if query
            != "endpoints=charge_state%3Bclimate_state%3Bclosures_state%3Bdrive_state%3Bgui_settings%3Blocation_data%3Bvehicle_config%3Bvehicle_state%3Bvehicle_data_combo"
        {
            return (
                StatusCode::BAD_REQUEST,
                r#"{"error":"vehicle_data endpoints query mismatch"}"#,
            )
                .into_response();
        }
        let response = state
            .data_bodies
            .lock()
            .expect("fake data lock")
            .get(&vehicle_id)
            .cloned()
            .unwrap_or((StatusCode::NOT_FOUND, r#"{"error":"not_found"}"#.to_owned()));
        response.into_response()
    }

    fn record(state: &FakeState, headers: &HeaderMap, path: &str) {
        record_with_query(state, headers, path, "");
    }

    fn record_with_query(state: &FakeState, headers: &HeaderMap, path: &str, query: &str) {
        let authorization_is_expected = headers
            .get("authorization")
            .is_some_and(|value| value.as_bytes() == b"Bearer test-owner-token");
        state
            .requests
            .lock()
            .expect("fake request lock")
            .push(FakeRequest {
                method: "GET".to_owned(),
                path: path.to_owned(),
                query: query.to_owned(),
                authorization_is_expected,
            });
    }

    #[test]
    fn retry_after_is_integer_exact_and_safe_on_bad_input() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("17"));
        assert_eq!(parse_retry_after(&headers), 17);

        headers.insert("retry-after", HeaderValue::from_static("bad"));
        assert_eq!(parse_retry_after(&headers), 300);
        headers.remove("retry-after");
        assert_eq!(parse_retry_after(&headers), 300);
    }

    #[test]
    fn exact_exceeded_limit_and_not_found_bodies_are_typed() {
        assert!(is_owner_error_body(
            br#"{"error":"account disabled: EXCEEDED_LIMIT"}"#,
            "account disabled: EXCEEDED_LIMIT"
        ));
        assert!(is_owner_error_body(
            br#"{"error":"not_found"}"#,
            "not_found"
        ));
        assert!(!is_owner_error_body(br#"{"error":"other"}"#, "not_found"));
    }
}
