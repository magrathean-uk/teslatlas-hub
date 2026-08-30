// SPDX-License-Identifier: AGPL-3.0-only

//! Optional, bounded reverse geocoding through an operator-selected,
//! Nominatim-compatible endpoint.
//!
//! It performs one request per call, has no internal retries, stores only a
//! bounded provider response, and keeps all provider errors free of
//! coordinates and response bodies.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use reqwest::{
    Client,
    header::{ACCEPT, ACCEPT_LANGUAGE, HeaderValue, USER_AGENT},
    redirect::Policy,
};
use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::{
    config::{ConfigError, GeocoderConfig},
    db::{AddressCacheRecord, HubStore, StoreError},
    location::Wgs84Point,
};

pub const FIXED_USER_AGENT: &str = concat!("teslatlas-hub/", env!("CARGO_PKG_VERSION"));
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_RAW_JSON_BYTES: usize = 64 * 1024;
const MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(1);

type SharedLimiter = Arc<Mutex<Option<Instant>>>;

/// A synchronous, last-moment permission check for optional outbound work.
///
/// The guard is deliberately evaluated by the code that owns the actual
/// request builder, immediately before `send`.  A caller must not replace it
/// with an earlier startup-only check: a macOS user admission can be revoked
/// while this module is waiting for its provider rate limit.
pub trait EgressGuard: Send + Sync {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError>;
}

/// Typed fail-closed result for an egress guard that no longer permits a
/// request.  Provider-specific errors intentionally remain separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("outbound request admission is no longer valid")]
pub struct EgressGuardError;

/// Reusable adapter for the admitted macOS Hub process marker.
///
/// It carries the marker itself, not a copied boolean, so every request
/// revalidates the process/session/installation ownership boundary.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub struct AdmittedUserEgressGuard {
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
}

#[cfg(unix)]
impl AdmittedUserEgressGuard {
    pub fn new(admission: Arc<crate::hub_user_process::AdmittedUserHub>) -> Self {
        Self { admission }
    }
}

#[cfg(unix)]
impl EgressGuard for AdmittedUserEgressGuard {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        self.admission
            .assert_sensitive_access()
            .map_err(|_| EgressGuardError)
    }
}

/// Permit the legacy Linux/test API paths that have no user-session admission
/// authority.  It is private so production macOS call sites must opt into a
/// real guard explicitly.
#[cfg(any(not(target_os = "macos"), test))]
pub(crate) struct UnguardedEgress;

#[cfg(any(not(target_os = "macos"), test))]
impl EgressGuard for UnguardedEgress {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeocodedAddress {
    pub osm_type: String,
    pub osm_id: i64,
    pub display_name: String,
    pub name: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub house_number: Option<String>,
    pub road: Option<String>,
    pub neighbourhood: Option<String>,
    pub city: Option<String>,
    pub county: Option<String>,
    pub postcode: Option<String>,
    pub state: Option<String>,
    pub state_district: Option<String>,
    pub country: Option<String>,
    pub raw_json: Option<String>,
}

impl From<AddressCacheRecord> for GeocodedAddress {
    fn from(record: AddressCacheRecord) -> Self {
        Self {
            osm_type: record.osm_type,
            osm_id: record.osm_id,
            display_name: record.display_name,
            name: record.name,
            latitude: record.latitude,
            longitude: record.longitude,
            house_number: record.house_number,
            road: record.road,
            neighbourhood: record.neighbourhood,
            city: record.city,
            county: record.county,
            postcode: record.postcode,
            state: record.state,
            state_district: record.state_district,
            country: record.country,
            raw_json: record.raw_json,
        }
    }
}

#[derive(Clone)]
pub struct Geocoder {
    client: Client,
    endpoint: Url,
    language: String,
    limiter: SharedLimiter,
}

impl std::fmt::Debug for Geocoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Geocoder")
            .field("endpoint", &self.endpoint)
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

impl Geocoder {
    pub fn new(config: &GeocoderConfig) -> Result<Self, GeocoderError> {
        Self::build(config, false, global_limiter())
    }

    #[cfg(test)]
    pub(crate) fn for_test(config: &GeocoderConfig) -> Result<Self, GeocoderError> {
        Self::build(config, true, Arc::new(Mutex::new(None)))
    }

    fn build(
        config: &GeocoderConfig,
        test_mode: bool,
        limiter: SharedLimiter,
    ) -> Result<Self, GeocoderError> {
        if !config.enabled {
            return Err(GeocoderError::Disabled);
        }
        let endpoint = config
            .endpoint_url(test_mode)
            .map_err(GeocoderError::InvalidConfig)?;
        let timeout = config.timeout().map_err(GeocoderError::InvalidConfig)?;
        let language = config
            .validated_language()
            .map_err(GeocoderError::InvalidConfig)?;

        crate::crypto::install_default_provider();
        let client = Client::builder()
            .https_only(!test_mode)
            .redirect(Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| GeocoderError::ClientBuild)?;

        Ok(Self {
            client,
            endpoint,
            language,
            limiter,
        })
    }

    #[cfg(any(not(target_os = "macos"), test))]
    pub async fn reverse(&self, point: Wgs84Point) -> Result<GeocodedAddress, GeocoderError> {
        self.reverse_with_egress_guard(point, &UnguardedEgress)
            .await
    }

    /// Reverse geocode after one final egress-admission check.
    ///
    /// This check comes after the shared rate-limit reservation, so a revoked
    /// user admission cannot issue a delayed request.
    pub async fn reverse_with_egress_guard<G: EgressGuard + ?Sized>(
        &self,
        point: Wgs84Point,
        egress_guard: &G,
    ) -> Result<GeocodedAddress, GeocoderError> {
        wait_for_slot(&self.limiter).await?;

        let url = self
            .endpoint
            .join("reverse")
            .map_err(|_| GeocoderError::InvalidEndpoint)?;
        let query = vec![
            ("format", "jsonv2".to_owned()),
            ("addressdetails", "1".to_owned()),
            ("extratags", "1".to_owned()),
            ("namedetails", "1".to_owned()),
            ("zoom", "19".to_owned()),
            ("lat", point.latitude.to_string()),
            ("lon", point.longitude.to_string()),
        ];
        let accept_language = HeaderValue::from_str(&self.language)
            .map_err(|_| GeocoderError::InvalidConfig(ConfigError::InvalidGeocoderLanguage))?;
        let user_agent = HeaderValue::from_static(FIXED_USER_AGENT);
        egress_guard
            .assert_egress_allowed()
            .map_err(|_| GeocoderError::EgressDenied)?;
        let response = self
            .client
            .get(url)
            .query(&query)
            .header(ACCEPT, "application/json")
            .header(ACCEPT_LANGUAGE, accept_language)
            .header(USER_AGENT, user_agent)
            .send()
            .await
            .map_err(classify_request_error)?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(GeocoderError::HttpStatus(status));
        }
        let body = read_limited_response(response).await?;
        parse_response(&body)
    }

    #[cfg(any(not(target_os = "macos"), test))]
    pub async fn reverse_cached(
        &self,
        store: &HubStore,
        point: Wgs84Point,
        looked_up_at_ms: i64,
    ) -> Result<GeocodedAddress, GeocoderError> {
        self.reverse_cached_with_egress_guard(store, point, looked_up_at_ms, &UnguardedEgress)
            .await
    }

    /// Cached reverse geocoding with the same final egress-admission check as
    /// [`Self::reverse_with_egress_guard`].
    pub async fn reverse_cached_with_egress_guard<G: EgressGuard + ?Sized>(
        &self,
        store: &HubStore,
        point: Wgs84Point,
        looked_up_at_ms: i64,
        egress_guard: &G,
    ) -> Result<GeocodedAddress, GeocoderError> {
        if let Some(cached) = store.cached_address(point).map_err(GeocoderError::Cache)? {
            return Ok(cached.into());
        }

        let address = self.reverse_with_egress_guard(point, egress_guard).await?;
        store
            .put_address_cache(&AddressCacheRecord {
                osm_type: address.osm_type.clone(),
                osm_id: address.osm_id,
                display_name: address.display_name.clone(),
                name: address.name.clone(),
                latitude: address.latitude,
                longitude: address.longitude,
                house_number: address.house_number.clone(),
                road: address.road.clone(),
                neighbourhood: address.neighbourhood.clone(),
                city: address.city.clone(),
                county: address.county.clone(),
                postcode: address.postcode.clone(),
                state: address.state.clone(),
                state_district: address.state_district.clone(),
                country: address.country.clone(),
                raw_json: address.raw_json.clone(),
                lookup_latitude: point.latitude,
                lookup_longitude: point.longitude,
                looked_up_at_ms,
            })
            .map_err(GeocoderError::Cache)?;
        Ok(address)
    }
}

fn global_limiter() -> SharedLimiter {
    static LIMITER: OnceLock<SharedLimiter> = OnceLock::new();
    LIMITER.get_or_init(|| Arc::new(Mutex::new(None))).clone()
}

async fn wait_for_slot(limiter: &SharedLimiter) -> Result<(), GeocoderError> {
    let now = Instant::now();
    let scheduled = {
        let mut last = limiter
            .lock()
            .map_err(|_| GeocoderError::RateLimiterPoisoned)?;
        let scheduled = last
            .map(|previous| previous + MIN_REQUEST_INTERVAL)
            .unwrap_or(now)
            .max(now);
        *last = Some(scheduled);
        scheduled
    };
    tokio::time::sleep(scheduled.saturating_duration_since(now)).await;
    Ok(())
}

async fn read_limited_response(response: reqwest::Response) -> Result<Vec<u8>, GeocoderError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(GeocoderError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| GeocoderError::ResponseRead)?;
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(GeocoderError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Debug, Deserialize)]
struct NominatimAddress {
    osm_type: Option<String>,
    osm_id: Option<i64>,
    display_name: Option<String>,
    name: Option<String>,
    namedetails: Option<BTreeMap<String, serde_json::Value>>,
    lat: Option<String>,
    lon: Option<String>,
    address: Option<NominatimAddressDetails>,
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct NominatimAddressDetails {
    #[serde(flatten)]
    fields: BTreeMap<String, serde_json::Value>,
}

impl NominatimAddressDetails {
    fn first(&self, names: &[&str]) -> Option<String> {
        names.iter().find_map(|name| {
            self.fields
                .get(*name)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
    }
}

fn parse_response(body: &[u8]) -> Result<GeocodedAddress, GeocoderError> {
    let raw: NominatimAddress =
        serde_json::from_slice(body).map_err(|_| GeocoderError::MalformedResponse)?;
    if raw.error.is_some() {
        return Err(GeocoderError::NoResult);
    }
    let osm_type = raw
        .osm_type
        .filter(|value| matches!(value.as_str(), "node" | "way" | "relation"))
        .ok_or(GeocoderError::MalformedResponse)?;
    let osm_id = raw
        .osm_id
        .filter(|value| *value > 0)
        .ok_or(GeocoderError::MalformedResponse)?;
    let display_name = raw
        .display_name
        .filter(|value| !value.trim().is_empty())
        .ok_or(GeocoderError::MalformedResponse)?
        .trim()
        .to_owned();
    let name = raw
        .name
        .or_else(|| {
            raw.namedetails.as_ref().and_then(|details| {
                ["name", "alt_name"].iter().find_map(|name| {
                    details
                        .get(*name)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
            })
        })
        .and_then(|value| (!value.trim().is_empty()).then(|| value.trim().to_owned()));
    let latitude = raw.lat.and_then(|value| value.parse::<f64>().ok());
    let longitude = raw.lon.and_then(|value| value.parse::<f64>().ok());
    let details = raw.address.unwrap_or_default();
    let raw_json = if body.len() <= MAX_RAW_JSON_BYTES {
        String::from_utf8(body.to_vec()).ok()
    } else {
        None
    };
    Ok(GeocodedAddress {
        osm_type,
        osm_id,
        display_name,
        name,
        latitude,
        longitude,
        house_number: details.first(&["house_number", "street_number"]),
        road: details.first(&[
            "road",
            "footway",
            "street",
            "street_name",
            "residential",
            "path",
            "pedestrian",
            "road_reference",
            "road_reference_intl",
            "square",
            "place",
        ]),
        neighbourhood: details.first(&[
            "neighbourhood",
            "suburb",
            "city_district",
            "district",
            "quarter",
            "borough",
            "city_block",
            "residential",
            "commercial",
            "houses",
            "subdistrict",
            "subdivision",
            "ward",
        ]),
        city: details.first(&[
            "city",
            "town",
            "township",
            "village",
            "municipality",
            "hamlet",
            "locality",
            "croft",
            "local_administrative_area",
            "subcounty",
        ]),
        county: details.first(&["county", "county_code", "department"]),
        postcode: details.first(&["postcode"]),
        state: details.first(&["state", "province", "territory", "state_code"]),
        state_district: details.first(&["state_district"]),
        country: details.first(&["country", "country_name"]),
        raw_json,
    })
}

fn classify_request_error(error: reqwest::Error) -> GeocoderError {
    if error.is_timeout() {
        GeocoderError::Timeout
    } else {
        GeocoderError::Transport
    }
}

#[derive(Debug, Error)]
pub enum GeocoderError {
    #[error("geocoder is disabled")]
    Disabled,
    #[error("invalid geocoder configuration: {0}")]
    InvalidConfig(ConfigError),
    #[error("geocoder client could not be created")]
    ClientBuild,
    #[error("geocoder endpoint is invalid")]
    InvalidEndpoint,
    #[error("geocoder request timed out")]
    Timeout,
    #[error("geocoder transport failed")]
    Transport,
    #[error("geocoder returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("geocoder response is too large")]
    ResponseTooLarge,
    #[error("geocoder response could not be read")]
    ResponseRead,
    #[error("geocoder response is malformed")]
    MalformedResponse,
    #[error("geocoder returned no result")]
    NoResult,
    #[error("geocoder rate limiter is unavailable")]
    RateLimiterPoisoned,
    #[error("geocoder egress admission is no longer valid")]
    EgressDenied,
    #[error("geocoder cache failed")]
    Cache(#[source] StoreError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        sync::{Arc, Mutex},
        thread,
    };

    use axum::{Router, body::Body, response::Response, routing::any};

    #[derive(Clone)]
    struct RevocableEgressGuard(Arc<AtomicBool>);

    impl EgressGuard for RevocableEgressGuard {
        fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
            self.0
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or(EgressGuardError)
        }
    }

    fn config(endpoint: String) -> GeocoderConfig {
        GeocoderConfig {
            enabled: true,
            endpoint: Some(endpoint),
            language: "en-GB".into(),
            timeout_seconds: 2,
        }
    }

    fn server(
        body: &'static str,
        requests: usize,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_thread = Arc::clone(&captured);
        let handle = thread::spawn(move || {
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                captured_thread
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(bytes).unwrap());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{address}"), captured, handle)
    }

    async fn counting_server() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let count_for_server = Arc::clone(&count);
        let app = Router::new().route(
            "/{*path}",
            any(move || {
                let count = Arc::clone(&count_for_server);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Response::new(Body::from(
                        r#"{"osm_type":"node","osm_id":7,"display_name":"Home"}"#,
                    ))
                }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (endpoint, count, task)
    }

    #[tokio::test]
    async fn sends_exact_nominatim_query_and_headers() {
        let (endpoint, captured, handle) = server(
            r#"{"osm_type":"way","osm_id":42,"display_name":"Main Road","name":"Main Road","lat":"51.5001","lon":"-0.1001","address":{"road":"Main Street","house_number":"7","city":"London","country":"United Kingdom"}}"#,
            1,
        );
        let geocoder = Geocoder::for_test(&config(endpoint)).unwrap();
        let result = geocoder
            .reverse(Wgs84Point::new(51.5, -0.1).unwrap())
            .await
            .unwrap();
        assert_eq!(result.osm_type, "way");
        assert_eq!(result.osm_id, 42);
        assert_eq!(result.latitude, Some(51.5001));
        assert_eq!(result.road.as_deref(), Some("Main Street"));
        assert_eq!(result.house_number.as_deref(), Some("7"));
        assert_eq!(result.city.as_deref(), Some("London"));
        assert!(result.raw_json.is_some());
        handle.join().unwrap();
        let request = captured.lock().unwrap().join("\n");
        assert!(request.contains("GET /reverse?format=jsonv2&addressdetails=1&extratags=1&namedetails=1&zoom=19&lat=51.5&lon=-0.1 HTTP/1.1"));
        assert!(request.contains("accept-language: en-GB"));
        assert!(request.contains(&format!("user-agent: {FIXED_USER_AGENT}")));
        assert!(!request.contains("ignored"));
    }

    #[tokio::test]
    async fn rejects_malformed_and_no_result_without_leaking_body() {
        for body in ["[]", r#"{"error":"Unable to geocode"}"#] {
            let (endpoint, _, handle) = server(body, 1);
            let geocoder = Geocoder::for_test(&config(endpoint)).unwrap();
            let error = geocoder
                .reverse(Wgs84Point::new(1.0, 2.0).unwrap())
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                GeocoderError::MalformedResponse | GeocoderError::NoResult
            ));
            assert!(!error.to_string().contains("1.0"));
            assert!(!error.to_string().contains("Unable"));
            handle.join().unwrap();
        }
    }

    #[tokio::test]
    async fn durable_cache_reuses_coordinate_lookup_without_second_request() {
        let (endpoint, _, handle) = server(
            r#"{"osm_type":"node","osm_id":7,"display_name":"Home","name":"Home","lat":"51.0001","lon":"-0.1001","address":{"road":"Home Road","neighbourhood":"Village","city":"London","postcode":"SW1A 1AA","state":"England","country":"United Kingdom"}}"#,
            1,
        );
        let temp = crate::private_tempdir().unwrap();
        let store = HubStore::initialize(temp.path()).unwrap();
        let geocoder = Geocoder::for_test(&config(endpoint)).unwrap();
        let point = Wgs84Point::new(51.0, -0.1).unwrap();
        let first = geocoder.reverse_cached(&store, point, 10).await.unwrap();
        assert_eq!(first.road.as_deref(), Some("Home Road"));
        assert_eq!(first.postcode.as_deref(), Some("SW1A 1AA"));
        drop(store);
        let reopened = HubStore::initialize(temp.path()).unwrap();
        let second = geocoder.reverse_cached(&reopened, point, 20).await.unwrap();
        assert_eq!(first, second);
        handle.join().unwrap();
        let connection = reopened.open().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM address_cache", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM address_lookup_cache", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            1
        );
    }

    #[test]
    fn parse_retains_teslamate_address_metadata() {
        let body = br#"{"osm_type":"relation","osm_id":9,"display_name":"9 Main Street","lat":"1.25","lon":"2.5","address":{"house_number":"9","road":"Main Street","neighbourhood":"Centre","city":"Test City","county":"Test County","postcode":"T1 2AB","state":"Test State","state_district":"Test District","country":"Testland"}}"#;
        let parsed = parse_response(body).unwrap();
        assert_eq!(parsed.latitude, Some(1.25));
        assert_eq!(parsed.longitude, Some(2.5));
        assert_eq!(parsed.house_number.as_deref(), Some("9"));
        assert_eq!(parsed.road.as_deref(), Some("Main Street"));
        assert_eq!(parsed.neighbourhood.as_deref(), Some("Centre"));
        assert_eq!(parsed.city.as_deref(), Some("Test City"));
        assert_eq!(parsed.county.as_deref(), Some("Test County"));
        assert_eq!(parsed.postcode.as_deref(), Some("T1 2AB"));
        assert_eq!(parsed.state.as_deref(), Some("Test State"));
        assert_eq!(parsed.state_district.as_deref(), Some("Test District"));
        assert_eq!(parsed.country.as_deref(), Some("Testland"));
        assert_eq!(
            parsed.raw_json,
            Some(String::from_utf8(body.to_vec()).unwrap())
        );
    }

    #[test]
    fn parse_uses_teslamate_v4_1_1_address_aliases() {
        let body = br#"{
            "osm_type":"relation","osm_id":10,"display_name":"Darwin NT",
            "namedetails":{"alt_name":"Darwin"},"lat":"-12.46","lon":"130.84",
            "address":{
                "street_number":"7","footway":"Esplanade Path","suburb":"Darwin City",
                "town":"Darwin","department":"Top End","postcode":"0800",
                "territory":"Northern Territory","country_name":"Australia"
            }
        }"#;
        let parsed = parse_response(body).expect("alias response");
        assert_eq!(parsed.name.as_deref(), Some("Darwin"));
        assert_eq!(parsed.house_number.as_deref(), Some("7"));
        assert_eq!(parsed.road.as_deref(), Some("Esplanade Path"));
        assert_eq!(parsed.neighbourhood.as_deref(), Some("Darwin City"));
        assert_eq!(parsed.city.as_deref(), Some("Darwin"));
        assert_eq!(parsed.county.as_deref(), Some("Top End"));
        assert_eq!(parsed.state.as_deref(), Some("Northern Territory"));
        assert_eq!(parsed.country.as_deref(), Some("Australia"));
    }

    #[tokio::test]
    async fn limiter_reserves_slots_one_second_apart() {
        let limiter = Arc::new(Mutex::new(None));
        wait_for_slot(&limiter).await.unwrap();
        let second_started = Instant::now();
        wait_for_slot(&limiter).await.unwrap();
        assert!(second_started.elapsed() >= Duration::from_millis(900));
    }

    #[tokio::test]
    async fn revoked_during_rate_wait_blocks_egress_before_send() {
        let (endpoint, requests, server_task) = counting_server().await;
        let reserved_at = Instant::now();
        let limiter = Arc::new(Mutex::new(Some(reserved_at)));
        let geocoder =
            Arc::new(Geocoder::build(&config(endpoint), true, Arc::clone(&limiter)).unwrap());
        let admitted = Arc::new(AtomicBool::new(true));
        let guard = RevocableEgressGuard(Arc::clone(&admitted));
        let task = tokio::spawn(async move {
            geocoder
                .reverse_with_egress_guard(Wgs84Point::new(51.5, -0.1).unwrap(), &guard)
                .await
        });

        loop {
            let scheduled = *limiter.lock().unwrap();
            if scheduled.is_some_and(|slot| slot > reserved_at + Duration::from_millis(500)) {
                break;
            }
            tokio::task::yield_now().await;
        }
        admitted.store(false, Ordering::Release);

        assert!(matches!(
            task.await.unwrap(),
            Err(GeocoderError::EgressDenied)
        ));
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        server_task.abort();
    }
}
