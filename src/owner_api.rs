//! Deliberate, one-shot compatibility reads from a Tesla Owner API endpoint.
//!
//! This is not a polling loop, a Fleet implementation, or a command client.
//! It only sends authenticated `GET` requests to the legacy-compatible product list and
//! `vehicle_data` paths. A vehicle is queried only if the preceding list call
//! reported it as `online`; the module never calls a wake endpoint.
//!
//! The owner token is a [`crate::credentials::OwnerToken`], which can only be
//! loaded from the service credential module. It is never accepted as a URL,
//! configuration string, environment value, or request query parameter.

use std::{fmt, time::Duration};

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

use crate::credentials::OwnerToken;

/// Four MiB is comfortably above a normal vehicle-data response while keeping
/// a bad upstream response from turning a manual collection into an unbounded
/// allocation.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const ACCEPT_JSON: HeaderValue = HeaderValue::from_static("application/json");

/// A validated, explicit HTTPS Owner API base URL.
///
/// There is intentionally no implicit production endpoint. The operator must
/// select a base URL during future collector configuration, making the legacy
/// compatibility boundary visible and reversible.
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
            return Err(OwnerApiConfigError::HttpsRequired);
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

    fn endpoint(&self, suffix: &str) -> Result<Url, OwnerApiError> {
        self.url
            .join(suffix)
            .map_err(|_| OwnerApiError::InvalidEndpoint)
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
pub struct OwnerApi {
    client: Client,
    base_url: OwnerApiBase,
}

impl OwnerApi {
    pub fn new(options: OwnerApiOptions) -> Result<Self, OwnerApiConfigError> {
        Self::build(options, false)
    }

    fn build(
        options: OwnerApiOptions,
        allow_insecure_test_base: bool,
    ) -> Result<Self, OwnerApiConfigError> {
        if options.request_timeout.is_zero() {
            return Err(OwnerApiConfigError::ZeroTimeout);
        }

        let client = Client::builder()
            .https_only(!allow_insecure_test_base)
            .redirect(Policy::none())
            .timeout(options.request_timeout)
            .build()
            .map_err(|_| OwnerApiConfigError::ClientBuild)?;

        Ok(Self {
            client,
            base_url: options.base_url,
        })
    }

    /// Discover account vehicles. This is a GET-only request and does not
    /// wake a vehicle.
    pub async fn list_vehicles(&self, token: &OwnerToken) -> Result<Vec<Vehicle>, OwnerApiError> {
        // Owner-token compatibility follows the current TeslaMate behavior:
        // discovery comes from `/products`. Fleet-specific `/vehicles` is not
        // silently substituted here.
        let envelope: ResponseEnvelope<Vec<ProductWire>> =
            self.get_envelope(token, "api/1/products").await?;

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

    /// Perform exactly one manual collection. Offline/asleep vehicles are
    /// reported but never queried. Per-vehicle data failures stay isolated so
    /// one unavailable car cannot discard another vehicle's snapshot.
    pub async fn collect_once(
        &self,
        token: &OwnerToken,
    ) -> Result<ManualCollection, OwnerApiError> {
        let vehicles = self.list_vehicles(token).await?;
        let mut snapshots = Vec::new();
        let mut failures = Vec::new();

        for vehicle in &vehicles {
            if !vehicle.is_online() {
                continue;
            }

            match self.vehicle_data(token, vehicle.id).await {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(error) => failures.push(VehicleCollectionFailure {
                    vehicle_id: vehicle.id,
                    error,
                }),
            }
        }

        Ok(ManualCollection {
            vehicles,
            snapshots,
            failures,
        })
    }

    /// Fetch one vehicle's reported state after [`Self::collect_once`] has
    /// confirmed it online. This is deliberately private so callers cannot
    /// turn this compatibility client into an arbitrary wake/poll mechanism.
    async fn vehicle_data(
        &self,
        token: &OwnerToken,
        vehicle_id: VehicleId,
    ) -> Result<VehicleData, OwnerApiError> {
        let suffix = format!("api/1/vehicles/{vehicle_id}/vehicle_data");
        let envelope: ResponseEnvelope<Map<String, Value>> =
            self.get_envelope(token, &suffix).await?;

        if envelope.count.is_some() || envelope.response.is_empty() {
            return Err(OwnerApiError::InvalidVehicleDataEnvelope);
        }
        if contains_sensitive_field(&envelope.response) {
            return Err(OwnerApiError::SensitiveDataInResponse);
        }

        Ok(VehicleData {
            vehicle_id,
            fields: envelope.response,
        })
    }

    async fn get_envelope<T>(
        &self,
        token: &OwnerToken,
        suffix: &str,
    ) -> Result<ResponseEnvelope<T>, OwnerApiError>
    where
        T: DeserializeOwned,
    {
        let url = self.base_url.endpoint(suffix)?;
        let response = self
            .client
            .get(url)
            .header(ACCEPT, ACCEPT_JSON.clone())
            .bearer_auth(token.as_str())
            .send()
            .await
            .map_err(classify_transport_error)?;

        if !response.status().is_success() {
            return Err(OwnerApiError::HttpStatus(response.status().as_u16()));
        }

        let bytes = read_limited_response(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| OwnerApiError::InvalidResponseEnvelope)
    }
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
}

impl fmt::Display for VehicleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Minimal, non-secret vehicle discovery data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vehicle {
    pub id: VehicleId,
    pub vin: String,
    pub state: String,
    pub display_name: Option<String>,
}

impl Vehicle {
    pub fn is_online(&self) -> bool {
        self.state == "online"
    }

    #[cfg(test)]
    pub(crate) fn for_test(id: u64, vin: &str, state: &str) -> Self {
        Self {
            id: VehicleId(id),
            vin: vin.to_owned(),
            state: state.to_owned(),
            display_name: None,
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
        vehicle_id.as_ref()?;

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
            vin,
            state,
            display_name,
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
        .filter(|id| *id != 0)
        .map(VehicleId)
        .ok_or(OwnerApiError::InvalidVehicleRecord)
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

fn contains_sensitive_field(fields: &Map<String, Value>) -> bool {
    fields.iter().any(|(key, value)| {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "access_token"
                | "refresh_token"
                | "authorization"
                | "token"
                | "tokens"
                | "backseat_token"
        ) || contains_sensitive_value(value)
    })
}

fn contains_sensitive_value(value: &Value) -> bool {
    match value {
        Value::Object(fields) => contains_sensitive_field(fields),
        Value::Array(values) => values.iter().any(contains_sensitive_value),
        _ => false,
    }
}

fn classify_transport_error(error: reqwest::Error) -> OwnerApiError {
    if error.is_timeout() {
        OwnerApiError::RequestTimeout
    } else {
        OwnerApiError::Transport
    }
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
    fn for_fake_http(
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
        fs,
        sync::{Arc, Mutex},
    };

    use axum::{
        Router,
        extract::{Path as AxumPath, State},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::get,
    };
    use tokio::{net::TcpListener, task::JoinHandle, time::sleep};

    use super::*;
    use crate::credentials::{CredentialDirectory, OWNER_TOKEN_CREDENTIAL};

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
        authorization_is_expected: bool,
    }

    impl FakeState {
        fn with_vehicles(body: &str) -> Self {
            Self {
                vehicles_body: Arc::new(Mutex::new(body.to_owned())),
                ..Self::default()
            }
        }

        fn set_data(&self, vehicle_id: u64, status: StatusCode, body: &str) {
            self.data_bodies
                .lock()
                .expect("fake data lock")
                .insert(vehicle_id.to_string(), (status, body.to_owned()));
        }
    }

    #[tokio::test]
    async fn collect_once_is_get_only_and_never_queries_an_unavailable_vehicle() {
        let state = FakeState::with_vehicles(&format!(
            r#"{{"response":[{{"id":1,"vehicle_id":10,"vin":"{TEST_VIN}","state":"online","tokens":["never-retained"]}},{{"id":"2","vehicle_id":20,"vin":"5YJ3E1EA7KF000002","state":"asleep"}},{{"energy_site_id":30,"product_type":"powerwall"}}],"count":3}}"#
        ));
        state.set_data(
            1,
            StatusCode::OK,
            r#"{"response":{"drive_state":{"timestamp":1},"vehicle_state":{"odometer":1.0}}}"#,
        );
        let fake = FakeServer::spawn(state.clone()).await;
        let client = fake.client(Duration::from_secs(2));
        let token = fake_owner_token();

        let collection = client.collect_once(&token).await.expect("collection");

        assert_eq!(collection.vehicles.len(), 2);
        assert_eq!(collection.snapshots.len(), 1);
        assert!(collection.failures.is_empty());
        assert_eq!(collection.snapshots[0].vehicle_id().get(), 1);
        let requests = state.requests.lock().expect("fake request lock");
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.method == "GET"));
        assert!(
            requests
                .iter()
                .all(|request| request.authorization_is_expected)
        );
        assert_eq!(requests[0].path, "/api/1/products");
        assert_eq!(requests[1].path, "/api/1/vehicles/1/vehicle_data");
    }

    #[tokio::test]
    async fn per_vehicle_data_failure_is_isolated() {
        let state = FakeState::with_vehicles(&format!(
            r#"{{"response":[{{"id":1,"vehicle_id":10,"vin":"{TEST_VIN}","state":"online"}},{{"id":2,"vehicle_id":20,"vin":"5YJ3E1EA7KF000002","state":"online"}}],"count":2}}"#
        ));
        state.set_data(
            1,
            StatusCode::OK,
            r#"{"response":{"vehicle_state":{"odometer":1.0}}}"#,
        );
        state.set_data(
            2,
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":"hidden-secret"}"#,
        );
        let fake = FakeServer::spawn(state).await;
        let client = fake.client(Duration::from_secs(2));

        let collection = client
            .collect_once(&fake_owner_token())
            .await
            .expect("collection continues");

        assert_eq!(collection.snapshots.len(), 1);
        assert_eq!(collection.failures.len(), 1);
        assert_eq!(collection.failures[0].vehicle_id.get(), 2);
        assert_eq!(collection.failures[0].error, OwnerApiError::HttpStatus(503));
    }

    #[tokio::test]
    async fn redirects_are_not_followed_or_replayed_with_the_bearer_token() {
        let state = FakeState::with_vehicles("redirect");
        let fake = FakeServer::spawn_redirecting(state.clone()).await;
        let client = fake.client(Duration::from_secs(2));

        let error = client
            .list_vehicles(&fake_owner_token())
            .await
            .expect_err("redirect is a non-success response");

        assert_eq!(error, OwnerApiError::HttpStatus(307));
        let requests = state.requests.lock().expect("fake request lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/api/1/products");
    }

    #[tokio::test]
    async fn strict_envelope_validation_and_errors_cannot_expose_a_token() {
        let state = FakeState::with_vehicles(r#"{"response":[],"count":1}"#);
        let fake = FakeServer::spawn(state).await;
        let client = fake.client(Duration::from_secs(2));
        let token = fake_owner_token();

        let error = client
            .list_vehicles(&token)
            .await
            .expect_err("count mismatch is rejected");
        assert_eq!(error, OwnerApiError::InvalidVehicleListCount);
        assert!(!error.to_string().contains(token.as_str()));
        assert!(!format!("{error:?}").contains(token.as_str()));
    }

    #[tokio::test]
    async fn response_with_credential_shaped_field_is_rejected_before_it_can_be_persisted() {
        let state = FakeState::with_vehicles(&format!(
            r#"{{"response":[{{"id":1,"vehicle_id":10,"vin":"{TEST_VIN}","state":"online"}}],"count":1}}"#
        ));
        state.set_data(
            1,
            StatusCode::OK,
            r#"{"response":{"drive_state":{"token":"do-not-store"}}}"#,
        );
        let fake = FakeServer::spawn(state).await;
        let client = fake.client(Duration::from_secs(2));

        let collection = client
            .collect_once(&fake_owner_token())
            .await
            .expect("per-vehicle error remains isolated");
        assert_eq!(collection.snapshots.len(), 0);
        assert_eq!(collection.failures.len(), 1);
        assert_eq!(
            collection.failures[0].error,
            OwnerApiError::SensitiveDataInResponse
        );
    }

    #[tokio::test]
    async fn request_timeout_is_bounded() {
        let fake = FakeServer::spawn_slow().await;
        let client = fake.client(Duration::from_millis(10));

        let error = client
            .list_vehicles(&fake_owner_token())
            .await
            .expect_err("slow response must respect timeout");
        assert_eq!(error, OwnerApiError::RequestTimeout);
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

    fn fake_owner_token() -> OwnerToken {
        let directory = tempfile::tempdir().expect("fake credential directory");
        let path = directory.path().join(OWNER_TOKEN_CREDENTIAL);
        fs::write(&path, TEST_TOKEN).expect("fake credential");
        set_private_mode(&path);
        CredentialDirectory::from_path(directory.path())
            .owner_token()
            .expect("typed token from credential module")
    }

    #[cfg(unix)]
    fn set_private_mode(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("private fake credential");
    }

    #[cfg(not(unix))]
    fn set_private_mode(_path: &std::path::Path) {}

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

        async fn spawn_redirecting(state: FakeState) -> Self {
            Self::start(
                Router::new()
                    .route("/api/1/products", get(redirect_handler))
                    .route("/redirect-capture", get(capture_redirect_handler))
                    .with_state(state),
            )
            .await
        }

        async fn spawn_slow() -> Self {
            Self::start(Router::new().route("/api/1/products", get(slow_handler))).await
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

    async fn list_handler(State(state): State<FakeState>, headers: HeaderMap) -> impl IntoResponse {
        record(&state, &headers, "/api/1/products");
        state.vehicles_body.lock().expect("fake list lock").clone()
    }

    async fn data_handler(
        State(state): State<FakeState>,
        AxumPath(vehicle_id): AxumPath<String>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        record(
            &state,
            &headers,
            &format!("/api/1/vehicles/{vehicle_id}/vehicle_data"),
        );
        state
            .data_bodies
            .lock()
            .expect("fake data lock")
            .get(&vehicle_id)
            .cloned()
            .unwrap_or((StatusCode::NOT_FOUND, r#"{"error":"not_found"}"#.to_owned()))
    }

    async fn redirect_handler(
        State(state): State<FakeState>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        record(&state, &headers, "/api/1/products");
        (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", "/redirect-capture")],
            "redirect",
        )
    }

    async fn capture_redirect_handler(
        State(state): State<FakeState>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        record(&state, &headers, "/redirect-capture");
        r#"{"response":[]}"#
    }

    async fn slow_handler() -> impl IntoResponse {
        sleep(Duration::from_millis(100)).await;
        r#"{"response":[]}"#
    }

    fn record(state: &FakeState, headers: &HeaderMap, path: &str) {
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
                authorization_is_expected,
            });
    }
}
