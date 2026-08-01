//! Optional, bounded Nominatim reverse geocoding.
//!
//! This module is not part of collection yet. It performs one request per
//! call, has no internal retries, never stores provider JSON, and keeps all
//! provider errors free of coordinates and response bodies.

use std::{
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

pub const DEFAULT_NOMINATIM_ENDPOINT: &str = "https://nominatim.openstreetmap.org";
pub const FIXED_USER_AGENT: &str = concat!("TeslaAtlas-Hub/", env!("CARGO_PKG_VERSION"));
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(1);

type SharedLimiter = Arc<Mutex<Option<Instant>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeocodedAddress {
    pub osm_type: String,
    pub osm_id: i64,
    pub display_name: String,
    pub name: Option<String>,
}

impl From<AddressCacheRecord> for GeocodedAddress {
    fn from(record: AddressCacheRecord) -> Self {
        Self {
            osm_type: record.osm_type,
            osm_id: record.osm_id,
            display_name: record.display_name,
            name: record.name,
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

    pub async fn reverse(&self, point: Wgs84Point) -> Result<GeocodedAddress, GeocoderError> {
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

    pub async fn reverse_cached(
        &self,
        store: &HubStore,
        point: Wgs84Point,
        looked_up_at_ms: i64,
    ) -> Result<GeocodedAddress, GeocoderError> {
        if let Some(cached) = store.cached_address(point).map_err(GeocoderError::Cache)? {
            return Ok(cached.into());
        }

        let address = self.reverse(point).await?;
        store
            .put_address_cache(&AddressCacheRecord {
                osm_type: address.osm_type.clone(),
                osm_id: address.osm_id,
                display_name: address.display_name.clone(),
                name: address.name.clone(),
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
    error: Option<serde_json::Value>,
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
        .and_then(|value| (!value.trim().is_empty()).then(|| value.trim().to_owned()));
    Ok(GeocodedAddress {
        osm_type,
        osm_id,
        display_name,
        name,
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
    #[error("geocoder cache failed")]
    Cache(#[source] StoreError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

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

    #[tokio::test]
    async fn sends_exact_nominatim_query_and_headers() {
        let (endpoint, captured, handle) = server(
            r#"{"osm_type":"way","osm_id":42,"display_name":"Main Road","name":"Main Road","address":{"road":"ignored"}}"#,
            1,
        );
        let geocoder = Geocoder::for_test(&config(endpoint)).unwrap();
        let result = geocoder
            .reverse(Wgs84Point::new(51.5, -0.1).unwrap())
            .await
            .unwrap();
        assert_eq!(result.osm_type, "way");
        assert_eq!(result.osm_id, 42);
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
            r#"{"osm_type":"node","osm_id":7,"display_name":"Home","name":"Home"}"#,
            1,
        );
        let temp = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(temp.path()).unwrap();
        let geocoder = Geocoder::for_test(&config(endpoint)).unwrap();
        let point = Wgs84Point::new(51.0, -0.1).unwrap();
        let first = geocoder.reverse_cached(&store, point, 10).await.unwrap();
        let second = geocoder.reverse_cached(&store, point, 20).await.unwrap();
        assert_eq!(first, second);
        handle.join().unwrap();
        let connection = store.open().unwrap();
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

    #[tokio::test]
    async fn limiter_reserves_slots_one_second_apart() {
        let limiter = Arc::new(Mutex::new(None));
        wait_for_slot(&limiter).await.unwrap();
        let second_started = Instant::now();
        wait_for_slot(&limiter).await.unwrap();
        assert!(second_started.elapsed() >= Duration::from_millis(900));
    }
}
