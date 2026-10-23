// SPDX-License-Identifier: AGPL-3.0-only

//! Optional, bounded reverse geocoding through an operator-selected,
//! Nominatim-compatible endpoint.
//!
//! It performs one request per call, has no internal retries, stores only a
//! bounded provider response, and keeps all provider errors free of
//! coordinates and response bodies.

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
use thiserror::Error;
use url::Url;

use crate::{
    config::{ConfigError, GeocoderConfig},
    db::{AddressCacheRecord, AddressLookupBinding, HubStore, StoreError},
    location::Wgs84Point,
};

mod provider_state;
mod request;
mod response_mapping;

pub(crate) use provider_state::{LookupGate, POLICY_URL, lookup_gate, provider_status};
#[cfg(test)]
use provider_state::{backoff_pause, lookup_gate_with};
use provider_state::{clear_state, load_state, record_refusal};

#[cfg(test)]
use request::is_public_host;
use request::{
    ACCEPT_JSON, RequestFailure, accept_language_value, request_interval, retry_after_seconds,
    reverse_request_url,
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
    /// The provider's licence text, verbatim, when it is not blank, is at most
    /// 512 bytes and has no control character; otherwise `None`.
    pub licence: Option<String>,
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
            licence: record.licence,
        }
    }
}

#[derive(Clone)]
pub struct Geocoder {
    client: Client,
    endpoint: Url,
    language: String,
    /// What every cached lookup of this geocoder is bound to: the provider,
    /// this endpoint and this language. A different endpoint or language is a
    /// different binding and never reads or rewrites this one's rows.
    binding: AddressLookupBinding,
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
        let binding = AddressLookupBinding::nominatim(&endpoint, &language)
            .map_err(|_| GeocoderError::InvalidEndpoint)?;

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
            binding,
            limiter,
        })
    }

    /// The provider, endpoint and language this geocoder's cache rows are
    /// bound to.
    pub fn lookup_binding(&self) -> &AddressLookupBinding {
        &self.binding
    }

    #[cfg(any(not(target_os = "macos"), test))]
    pub async fn reverse(&self, point: Wgs84Point) -> Result<GeocodedAddress, GeocoderError> {
        self.reverse_with_egress_guard(point, &UnguardedEgress)
            .await
    }

    /// Reverse geocode with an egress-admission check before the rate wait and a
    /// final one after the slot is reserved.
    ///
    /// The final check comes after the shared rate-limit reservation, so a
    /// revoked user admission cannot issue a delayed request.
    pub async fn reverse_with_egress_guard<G: EgressGuard + ?Sized>(
        &self,
        point: Wgs84Point,
        egress_guard: &G,
    ) -> Result<GeocodedAddress, GeocoderError> {
        self.send_reverse(point, egress_guard)
            .await
            .map_err(|failure| failure.error)
    }

    /// C7 R1 and R53: the whole request path. It returns at the first failure and
    /// never retries; a refusal status 403 or 429 also carries the provider's
    /// requested delay for the caller's back-off.
    async fn send_reverse<G: EgressGuard + ?Sized>(
        &self,
        point: Wgs84Point,
        egress_guard: &G,
    ) -> Result<GeocodedAddress, RequestFailure> {
        // C7 R1 step 1: the URL, the header value and, in test builds, the public-host guard.
        let url = reverse_request_url(&self.endpoint, point, &self.language)?;
        #[cfg(test)]
        if is_public_host(&self.endpoint) {
            return Err(GeocoderError::InvalidEndpoint.into());
        }
        let accept_language = accept_language_value(&self.language)?;

        // C7 R1 steps 2 to 4: admission, the reserved slot, admission again.
        admit(egress_guard)?;
        wait_for_slot_every(&self.limiter, request_interval(&self.endpoint)).await?;
        admit(egress_guard)?;

        // C7 R1 step 5: one GET with exactly three headers (R13, R14).
        let response = self
            .client
            .get(url)
            .header(USER_AGENT, HeaderValue::from_static(FIXED_USER_AGENT))
            .header(ACCEPT, HeaderValue::from_static(ACCEPT_JSON))
            .header(ACCEPT_LANGUAGE, accept_language)
            .send()
            .await
            .map_err(classify_request_error)?;

        // C7 R1 step 6 and R16: the status comes before any body byte is read.
        let status = response.status();
        if !status.is_success() {
            let code = status.as_u16();
            let retry_after_seconds = if code == 403 || code == 429 {
                retry_after_seconds(response.headers())
            } else {
                None
            };
            return Err(RequestFailure {
                error: GeocoderError::HttpStatus(code),
                retry_after_seconds,
            });
        }

        // C7 R1 steps 7 and 8.
        let body = read_limited_response(response).await?;
        Ok(parse_response(&body)?)
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

    /// Cached reverse geocoding with the same egress-admission checks as
    /// [`Self::reverse_with_egress_guard`], and one more before the cache write.
    ///
    /// C7 R59 to R62: after a cache miss it returns `BackingOff` while the provider's
    /// refusal pause is in force, stores a refusal (403, 429) as the durable provider
    /// state, and clears that state on the first answer that is not a refusal.
    pub async fn reverse_cached_with_egress_guard<G: EgressGuard + ?Sized>(
        &self,
        store: &HubStore,
        point: Wgs84Point,
        looked_up_at_ms: i64,
        egress_guard: &G,
    ) -> Result<GeocodedAddress, GeocoderError> {
        if let Some(cached) = store
            .cached_address(&self.binding, point)
            .map_err(GeocoderError::Cache)?
        {
            return Ok(cached.into());
        }

        // C7 R59: no admission check, rate wait or request while a pause is in force.
        // An unreadable record fails closed.
        let state = load_state(store).map_err(GeocoderError::Cache)?;
        if state.as_ref().is_some_and(|state| {
            state
                .pause_end(&self.binding.endpoint, looked_up_at_ms)
                .is_some()
        }) {
            return Err(GeocoderError::BackingOff);
        }

        let address = match self.send_reverse(point, egress_guard).await {
            Ok(address) => {
                self.clear_provider_state(store, state.as_ref(), egress_guard);
                address
            }
            Err(failure) => {
                match failure.error {
                    // C7 R58: the provider refuses this client now.
                    GeocoderError::HttpStatus(status @ (403 | 429)) => {
                        // The state row is a store mutation after the request's
                        // awaits, so the guard stands in front of it too.
                        admit(egress_guard)?;
                        record_refusal(
                            store,
                            &self.binding,
                            state.as_ref(),
                            status,
                            failure.retry_after_seconds,
                            looked_up_at_ms,
                        )?;
                    }
                    // C7 R62: the provider answered, with something other than a refusal.
                    GeocoderError::HttpStatus(_)
                    | GeocoderError::ResponseTooLarge
                    | GeocoderError::ResponseRead
                    | GeocoderError::MalformedResponse
                    | GeocoderError::NoResult => {
                        self.clear_provider_state(store, state.as_ref(), egress_guard);
                    }
                    _ => {}
                }
                return Err(failure.error);
            }
        };
        // The cache row is a store mutation after the lookup's awaits: the guard that
        // stood in front of the request stands in front of it too (B2 fix round, P3b
        // fix 2, R-P3bF2-1).
        egress_guard
            .assert_egress_allowed()
            .map_err(|_| GeocoderError::EgressDenied)?;
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
                binding: self.binding.clone(),
                licence: address.licence.clone(),
                lookup_latitude: point.latitude,
                lookup_longitude: point.longitude,
                looked_up_at_ms,
            })
            .map_err(GeocoderError::Cache)?;
        Ok(address)
    }
}

impl Geocoder {
    /// C7 R62: delete the stored provider state, if there is one, after the provider
    /// answered with something other than a refusal. A refused guard leaves the row
    /// for the next answer; the cache write that follows is refused by the same guard.
    fn clear_provider_state<G: EgressGuard + ?Sized>(
        &self,
        store: &HubStore,
        state: Option<&provider_state::ProviderState>,
        egress_guard: &G,
    ) {
        if let Some(state) = state
            && admit(egress_guard).is_ok()
        {
            clear_state(store, state);
        }
    }
}

#[cfg(test)]
thread_local! {
    /// A test thread's own stand-in for the process-wide limiter.
    static THREAD_LIMITER: std::cell::RefCell<Option<SharedLimiter>> =
        const { std::cell::RefCell::new(None) };
}

/// Give the calling test thread a process-wide limiter of its own, holding no
/// slot, and return it. A test that runs the enrichment step reaches the
/// geocoder and the pre-claim gate through [`global_limiter`]; without this the
/// seeded process-wide limiter (C7 R55) would close the gate for one interval
/// after the first use, and another test's reserved slot would too.
#[cfg(test)]
pub(crate) fn isolate_global_limiter_for_test() -> SharedLimiter {
    let limiter: SharedLimiter = Arc::new(Mutex::new(None));
    THREAD_LIMITER.with(|own| *own.borrow_mut() = Some(Arc::clone(&limiter)));
    limiter
}

fn global_limiter() -> SharedLimiter {
    #[cfg(test)]
    if let Some(own) = THREAD_LIMITER.with(|own| own.borrow().clone()) {
        return own;
    }
    static LIMITER: OnceLock<SharedLimiter> = OnceLock::new();
    LIMITER
        .get_or_init(|| seeded_limiter(Instant::now()))
        .clone()
}

/// C7 R55: a limiter that behaves as if a request slot started at `instant`, so
/// the first request of a process waits one full interval and the spacing also
/// holds across a restart.
fn seeded_limiter(instant: Instant) -> SharedLimiter {
    Arc::new(Mutex::new(Some(instant)))
}

/// C7 R55: whether a request could start at `now` without waiting. It reads the
/// limiter and reserves nothing.
fn slot_free(
    limiter: &SharedLimiter,
    interval: Duration,
    now: Instant,
) -> Result<bool, GeocoderError> {
    let last = limiter
        .lock()
        .map_err(|_| GeocoderError::RateLimiterPoisoned)?;
    Ok(last.is_none_or(|previous| {
        previous
            .checked_add(interval)
            .is_some_and(|next_slot| next_slot <= now)
    }))
}

/// C7 R4: the default one-second interval, `wait_for_slot_every` with
/// `MIN_REQUEST_INTERVAL`. The request path calls `wait_for_slot_every` with the
/// endpoint's interval, so only the kept spacing test calls this.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the request path uses wait_for_slot_every")
)]
async fn wait_for_slot(limiter: &SharedLimiter) -> Result<(), GeocoderError> {
    wait_for_slot_every(limiter, MIN_REQUEST_INTERVAL).await
}

/// C7 R4: reserve the slot `max(previous slot + interval, now)` under the lock,
/// then sleep until it. The lock is never held across the sleep and a reserved
/// slot is never given back.
async fn wait_for_slot_every(
    limiter: &SharedLimiter,
    interval: Duration,
) -> Result<(), GeocoderError> {
    let now = Instant::now();
    let scheduled = {
        let mut last = limiter
            .lock()
            .map_err(|_| GeocoderError::RateLimiterPoisoned)?;
        let scheduled = last
            .map(|previous| previous + interval)
            .unwrap_or(now)
            .max(now);
        *last = Some(scheduled);
        scheduled
    };
    tokio::time::sleep(scheduled.saturating_duration_since(now)).await;
    Ok(())
}

/// C7 R3: the last-moment admission check, as the geocoder's own error.
fn admit<G: EgressGuard + ?Sized>(egress_guard: &G) -> Result<(), GeocoderError> {
    egress_guard
        .assert_egress_allowed()
        .map_err(|_| GeocoderError::EgressDenied)
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

/// The response mapping of the request path: the one pure function in
/// `response_mapping`.
fn parse_response(body: &[u8]) -> Result<GeocodedAddress, GeocoderError> {
    response_mapping::parse_response(body)
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
    /// C7 R59: the provider refused this client and its pause is still in force.
    #[error("geocoder provider refused requests; waiting before the next attempt")]
    BackingOff,
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

    // Reverse-geocoder request, admission and pacing tests (C7 part A).
    #[path = "b3_c7_a_tests.rs"]
    mod b3_c7_a;

    // Reverse-geocoder response mapping tests (C7 part B).
    #[path = "b3_c7_b_tests.rs"]
    mod b3_c7_b;

    // Reverse-geocoder defaults, durable refusal and pre-claim gate tests (C7 part C).
    #[path = "b3_c7_c_tests.rs"]
    mod b3_c7_c;

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
    async fn rejects_malformed_and_no_result_without_leaking_body() {
        // A top-level array is not a reverse answer at all: `ResponseRead`, which the caller
        // retries (R21). An `error` member is `NoResult` (R23).
        for (body, expected) in [
            ("[]", "ResponseRead"),
            (r#"{"error":"Unable to geocode"}"#, "NoResult"),
        ] {
            let (endpoint, _, handle) = server(body, 1);
            let geocoder = Geocoder::for_test(&config(endpoint)).unwrap();
            let error = geocoder
                .reverse(Wgs84Point::new(1.0, 2.0).unwrap())
                .await
                .unwrap_err();
            assert_eq!(format!("{error:?}"), expected, "{body}");
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

#[cfg(test)]
#[path = "geocoder_cache_tests.rs"]
mod cache_tests;
