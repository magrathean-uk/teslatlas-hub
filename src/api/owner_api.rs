// SPDX-License-Identifier: AGPL-3.0-only

//! Deliberate compatibility requests for a Tesla Owner API endpoint.
//!
//! This is not a polling loop or a Fleet implementation. It sends authenticated
//! reads to the legacy-compatible product list and crate-local `vehicle_data`
//! paths. Vehicle commands are available only through explicit one-shot calls;
//! the collector never invokes them.
//!
//! Legacy authentication is supplied only through typed credential owners;
//! raw bearer strings never enter the production API.

use std::{
    fmt,
    net::IpAddr,
    time::{Duration, SystemTime},
};

use futures_util::StreamExt;
use reqwest::{
    Client,
    header::{ACCEPT, CONTENT_TYPE, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use thiserror::Error;
use url::Url;

use crate::{
    credentials::{LegacyAuthManager, LegacyAuthManagerError},
    hub_pack::ProjectionCarSettings,
    legacy_auth::{LegacyAuth, LegacyAuthError, LegacyAuthFuse},
    tesla_stream::{StreamPowerGate, StreamRegion},
};

mod legacy_requests;
use legacy_requests::AnswerMeta;
pub(crate) mod provider_secrets;
pub use provider_secrets::ProviderSecrets;

/// Four MiB is comfortably above a normal vehicle-data response while keeping
/// a bad upstream response from turning a manual collection into an unbounded
/// allocation.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_PROVIDER_TELEMETRY_TEXT_BYTES: usize = 512;
const ACCEPT_JSON: HeaderValue = HeaderValue::from_static("application/json");

/// The most of an error response body that the status rules read (C3 R42): the
/// first 4,096 bytes, as the Fleet-naming test names. A bound this small keeps
/// the scan of a hostile or accidental large error page cheap, and the body is
/// never kept, parsed beyond its `error` strings or logged.
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 4_096;

/// The text of Tesla's 405 refusal of a car in service (C3 R42, R52), matched
/// ASCII case-insensitively: "405 whose error text contains \"in service\"".
const IN_SERVICE_TEXT: &str = "in service";

/// The text, reduced to ASCII letters and digits, that makes a 403 body
/// Fleet-named (C3 R42): it accepts "Fleet API", "fleet-api" and "fleet_api"
/// alike, and a documentation link that names the API.
const FLEET_API_NAME: &str = "fleetapi";

/// The `vehicle_data` endpoint groups the Hub's data model uses (C3 R23), in
/// Tesla's semicolon-separated `endpoints` form and in alphabetical order so
/// request logs and test vectors are stable. The same constant serves the
/// Owner-wire and Fleet requests; the query builder encodes the semicolons as
/// `%3B`. One group per Hub need:
///
/// - `charge_state`: charging state, levels, ranges, energy added, charger
///   power, voltage, current and phases (charge sessions, poll phase);
/// - `climate_state`: temperatures, set points, `is_climate_on` and
///   `climate_keeper_mode` (use evidence, C3 R29);
/// - `drive_state`: gear, speed, power, heading and the group timestamp
///   (drive evidence and folds, poll phase);
/// - `location_data`: latitude and longitude, which Tesla returns only for
///   this group on firmware 2023.38 and later;
/// - `vehicle_config`: `car_type` and `trim_badging` for the car's model
///   metadata, in the same request so it costs nothing extra;
/// - `vehicle_state`: odometer, `car_version`, `software_update`,
///   `service_mode`, tyre pressures, `locked`, `is_user_present`,
///   `remote_start` and `sentry_mode`.
///
/// Not requested: `gui_settings` (Tesla returns ranges in miles and speeds in
/// mph whatever the display setting and the Hub converts with fixed factors,
/// so display preferences are not data, and no published field is fed only
/// from it), `closures_state`, `charge_schedule_data`,
/// `preconditioning_schedule_data` and `vehicle_data_combo`.
pub(crate) const VEHICLE_DATA_ENDPOINTS: &str =
    "charge_state;climate_state;drive_state;location_data;vehicle_config;vehicle_state";

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

    /// Hermetic loopback-only constructor. Production configuration always
    /// goes through `parse`, which requires HTTPS before any bearer client is
    /// built.
    #[cfg(test)]
    pub(crate) fn parse_loopback_http_for_test(value: &str) -> Result<Self, OwnerApiConfigError> {
        let url = Url::parse(value).map_err(|_| OwnerApiConfigError::InvalidBaseUrl)?;
        let base = Self::from_url(url, false)?;
        if !base.is_loopback_http() {
            return Err(OwnerApiConfigError::HttpsRequired);
        }
        Ok(base)
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
            .field(&"[redacted]")
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
    host.and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .is_some_and(|address| address.is_loopback())
}

impl OwnerApi {
    pub(crate) fn new(options: OwnerApiOptions) -> Result<Self, OwnerApiConfigError> {
        Self::build(options, false)
    }

    pub(crate) fn legacy_auth_http_client(&self) -> Client {
        self.legacy_auth_client.clone()
    }

    fn build(
        options: OwnerApiOptions,
        allow_insecure_test_base: bool,
    ) -> Result<Self, OwnerApiConfigError> {
        if options.base_url.url.scheme() != "https"
            && !(allow_insecure_test_base && options.base_url.is_loopback_http())
        {
            return Err(OwnerApiConfigError::HttpsRequired);
        }
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
        // The token endpoint is a separate client with its own policy: no
        // redirects, which would resend the refresh body to a host nobody
        // chose, and its own timeouts (C5 R11, R12, R33).
        let legacy_auth_client = crate::legacy_auth::build_auth_client(allow_insecure_test_base)
            .map_err(|_| OwnerApiConfigError::ClientBuild)?;

        Ok(Self {
            client,
            legacy_auth_client,
            base_url: options.base_url,
        })
    }

    /// Native first-run discovery performs one bounded products request with
    /// the supplied in-memory pair. It never refreshes, wakes, or commands a
    /// vehicle; persistence happens only after the caller selects a car.
    pub(crate) async fn list_vehicles_with_legacy_auth_once(
        &self,
        auth: &LegacyAuth,
    ) -> Result<Vec<Vehicle>, OwnerApiError> {
        let endpoint = self.base_url.endpoint("api/1/products")?;
        let envelope: ResponseEnvelope<Vec<ProductWire>> = self
            .execute_envelope_request(self.envelope_request(auth.access_token(), endpoint))
            .await?;
        let mut vehicles = parse_vehicle_list(envelope)?;
        ProviderSecrets::new(&[auth.access_token(), auth.refresh_token()])
            .scrub_vehicle_list(&mut vehicles);
        Ok(vehicles)
    }

    /// Execute one explicit legacy Owner API action without refresh or retry.
    /// Test seam for the exact command wire contract.
    #[cfg(test)]
    pub(crate) async fn execute_vehicle_action_once(
        &self,
        auth: &LegacyAuth,
        vehicle_id: VehicleId,
        action: LegacyVehicleAction,
    ) -> Result<LegacyVehicleActionResult, OwnerApiError> {
        let endpoint = self.vehicle_action_endpoint(vehicle_id, action)?;
        let body = action.request_body_bytes()?;
        let request = self
            .client
            .post(endpoint)
            .header(ACCEPT, ACCEPT_JSON.clone())
            .header(CONTENT_TYPE, ACCEPT_JSON.clone())
            .bearer_auth(auth.access_token())
            .body(body);
        self.execute_vehicle_action_request(request, action).await
    }

    fn vehicle_action_endpoint(
        &self,
        vehicle_id: VehicleId,
        action: LegacyVehicleAction,
    ) -> Result<Url, OwnerApiError> {
        if matches!(action, LegacyVehicleAction::SetChargeLimit(percent) if !(50..=100).contains(&percent))
        {
            return Err(OwnerApiError::InvalidCommand);
        }
        self.base_url.endpoint(&format!(
            "api/1/vehicles/{vehicle_id}/{}",
            action.endpoint_suffix()
        ))
    }

    #[cfg(test)]
    async fn execute_vehicle_action_request(
        &self,
        request: reqwest::RequestBuilder,
        action: LegacyVehicleAction,
    ) -> Result<LegacyVehicleActionResult, OwnerApiError> {
        self.execute_vehicle_action_request_with(request, action, &mut AnswerMeta::default())
            .await
    }

    async fn execute_vehicle_action_request_with(
        &self,
        request: reqwest::RequestBuilder,
        action: LegacyVehicleAction,
        meta: &mut AnswerMeta,
    ) -> Result<LegacyVehicleActionResult, OwnerApiError> {
        match action {
            LegacyVehicleAction::Wake => {
                let envelope: ResponseEnvelope<Map<String, Value>> =
                    self.execute_envelope_request_with(request, meta).await?;
                if envelope.count.is_some() || envelope.response.is_empty() {
                    return Err(OwnerApiError::InvalidCommandResponse);
                }
                let state = envelope
                    .response
                    .get("state")
                    .and_then(Value::as_str)
                    .filter(|state| valid_state(state))
                    .map(str::to_owned);
                Ok(LegacyVehicleActionResult { state })
            }
            _ => {
                let envelope: ResponseEnvelope<CommandResponseWire> =
                    self.execute_envelope_request_with(request, meta).await?;
                if envelope.count.is_some() {
                    return Err(OwnerApiError::InvalidCommandResponse);
                }
                if !envelope.response.result {
                    return Err(OwnerApiError::CommandRejected);
                }
                Ok(LegacyVehicleActionResult { state: None })
            }
        }
    }

    fn vehicle_data_endpoint(&self, vehicle_id: VehicleId) -> Result<Url, OwnerApiError> {
        let mut endpoint = self
            .base_url
            .endpoint(&format!("api/1/vehicles/{vehicle_id}/vehicle_data"))?;
        endpoint
            .query_pairs_mut()
            .append_pair("endpoints", VEHICLE_DATA_ENDPOINTS);
        Ok(endpoint)
    }

    fn vehicle_probe_endpoint(&self, vehicle_id: VehicleId) -> Result<Url, OwnerApiError> {
        self.base_url
            .endpoint(&format!("api/1/vehicles/{vehicle_id}"))
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
        self.execute_envelope_request_with(request, &mut AnswerMeta::default())
            .await
    }
}

/// The error status of an Owner-wire response as a typed error (C3 R42), read
/// once, in whole, from the status and headers and, for the two statuses whose
/// meaning depends on the words of the body, from at most
/// [`MAX_ERROR_BODY_BYTES`] of it:
///
/// - 429 is `RateLimited` with the wait the headers ask for (R45);
/// - 404 is `VehicleNotFound`;
/// - 410 is `Gone`, RFC 9110's "no longer available, likely permanent";
/// - 403 is `Forbidden`, with whether its body names the Fleet API (R42);
/// - 405 is `VehicleInService` when its error text says so, else a plain
///   status;
/// - every other status, 401 and 408 included, stays `HttpStatus`, so the
///   classifier reads the number and no caller needs a variant.
///
/// Neither the body nor a header other than the numeric wait leaves this
/// function (C3 R9).
async fn owner_error_for_response(response: reqwest::Response) -> OwnerApiError {
    let status = response.status().as_u16();
    match status {
        429 => OwnerApiError::RateLimited {
            retry_after_seconds: parse_retry_after(response.headers()),
        },
        404 => OwnerApiError::VehicleNotFound,
        410 => OwnerApiError::Gone,
        403 => OwnerApiError::Forbidden {
            fleet_api_named: is_fleet_api_named(&read_error_body(response).await),
        },
        405 => {
            if is_vehicle_in_service_body(&read_error_body(response).await) {
                OwnerApiError::VehicleInService
            } else {
                OwnerApiError::HttpStatus(405)
            }
        }
        _ => OwnerApiError::HttpStatus(status),
    }
}

/// At most [`MAX_ERROR_BODY_BYTES`] of an error response body. A body that
/// cannot be read, or is longer, simply yields what was read: an error body only
/// ever refines a status that is already known.
async fn read_error_body(response: reqwest::Response) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while bytes.len() < MAX_ERROR_BODY_BYTES {
        let Some(Ok(chunk)) = stream.next().await else {
            break;
        };
        let room = MAX_ERROR_BODY_BYTES - bytes.len();
        bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
    bytes
}

/// The strings of an error body that the status rules read (C3 R42): for a JSON
/// object, its `error` and `error_description` strings, each on its own so a
/// word cannot form across their boundary; for anything else, the first
/// [`MAX_ERROR_BODY_BYTES`] bytes as text.
fn error_body_texts(bytes: &[u8]) -> Vec<String> {
    let bytes = &bytes[..bytes.len().min(MAX_ERROR_BODY_BYTES)];
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(fields)) => ["error", "error_description"]
            .into_iter()
            .filter_map(|key| fields.get(key).and_then(Value::as_str))
            .map(str::to_owned)
            .collect(),
        _ => vec![String::from_utf8_lossy(bytes).into_owned()],
    }
}

/// Whether an error body's text contains `expected`, ASCII case-insensitively
/// (C3 R42). The text is matched in memory and never logged.
fn is_owner_error_body(bytes: &[u8], expected: &str) -> bool {
    let expected = expected.to_ascii_lowercase();
    error_body_texts(bytes)
        .iter()
        .any(|text| text.to_ascii_lowercase().contains(&expected))
}

/// The one test that a 403 body names the Fleet API (C3 R42), shared with the
/// token endpoint's classification (C5 R45 row 10) so the two cannot disagree
/// about the same words: the body's text, lowercased and reduced to ASCII
/// letters and digits, contains `fleetapi`.
pub(crate) fn is_fleet_api_named(bytes: &[u8]) -> bool {
    error_body_texts(bytes).iter().any(|text| {
        text.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|character| character.to_ascii_lowercase())
            .collect::<String>()
            .contains(FLEET_API_NAME)
    })
}

/// C3 R45 `K_RETRY_AFTER_MAX`: the longest wait a `Retry-After` or
/// `RateLimit-Reset` header may ask for, one hour. It bounds the damage of a
/// wrong or hostile header: one request an hour then re-asks.
pub(crate) const RETRY_AFTER_MAX_SECONDS: u64 = 3_600;

/// The wait a 429 response asks for, in whole seconds, or 0 when it names none
/// (C3 R45). Shared by the Owner and Fleet clients.
pub(crate) fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> u64 {
    retry_after_seconds_at(headers, SystemTime::now())
}

/// [`parse_retry_after`] against a given wall time.
///
/// RFC 9110 section 10.2.3: `Retry-After` is `delay-seconds` (ASCII digits, up
/// to `u64`) or an HTTP-date (IMF-fixdate, and the two obsolete forms section
/// 5.6.7 requires recipients to accept), whose wait is the date minus `now`
/// rounded up to whole seconds. When `Retry-After` is absent or invalid,
/// `RateLimit-Reset` is used the same way if it is `delay-seconds` (Tesla names
/// both headers). A result of 0, a date that is not in the future or no usable
/// header returns 0, meaning "absent", because an immediate retry lands in the
/// same rate-limit window. Larger values are clamped to
/// [`RETRY_AFTER_MAX_SECONDS`].
pub(crate) fn retry_after_seconds_at(headers: &reqwest::header::HeaderMap, now: SystemTime) -> u64 {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
    };
    header("retry-after")
        .and_then(|value| retry_after_wait(value, now, true))
        .or_else(|| header("ratelimit-reset").and_then(|value| retry_after_wait(value, now, false)))
        .unwrap_or(0)
        .min(RETRY_AFTER_MAX_SECONDS)
}

fn retry_after_wait(value: &str, now: SystemTime, allow_http_date: bool) -> Option<u64> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value.parse::<u64>().ok();
    }
    if !allow_http_date {
        return None;
    }
    let now_ms =
        i128::try_from(now.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_millis()).ok()?;
    let date_ms = i128::from(parse_http_date(value, now_ms)?) * 1_000;
    let wait_ms = date_ms - now_ms;
    if wait_ms <= 0 {
        return Some(0);
    }
    u64::try_from((wait_ms + 999) / 1_000).ok()
}

/// HTTP-date to Unix seconds (RFC 9110 section 5.6.7): IMF-fixdate
/// (`Sun, 06 Nov 1994 08:49:37 GMT`), rfc850-date (`Sunday, 06-Nov-94
/// 08:49:37 GMT`) and asctime-date (`Sun Nov  6 08:49:37 1994`). A two-digit
/// year more than 50 years ahead of `now_ms` is read as the most recent past
/// year with the same last two digits, compared by calendar year. The weekday
/// must be a valid name; it is not checked against the date.
fn parse_http_date(value: &str, now_ms: i128) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const SHORT_DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const LONG_DAYS: [&str; 7] = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    let month_number = |name: &str| {
        MONTHS
            .iter()
            .position(|month| *month == name)
            .and_then(|index| i64::try_from(index + 1).ok())
    };
    let digits = |text: &str, length: usize| -> Option<i64> {
        (text.len() == length && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    };
    let parts: Vec<&str> = value.split_whitespace().collect();
    let (day, month, year, time) = match parts.as_slice() {
        [weekday, day, month, year, time, "GMT"]
            if weekday
                .strip_suffix(',')
                .is_some_and(|name| SHORT_DAYS.contains(&name)) =>
        {
            (
                digits(day, 2)?,
                month_number(month)?,
                digits(year, 4)?,
                *time,
            )
        }
        [weekday, date, time, "GMT"]
            if weekday
                .strip_suffix(',')
                .is_some_and(|name| LONG_DAYS.contains(&name)) =>
        {
            let mut fields = date.split('-');
            let (day, month, short_year) = (fields.next()?, fields.next()?, fields.next()?);
            if fields.next().is_some() {
                return None;
            }
            let now_year = civil_from_days(i64::try_from(now_ms.div_euclid(86_400_000)).ok()?).0;
            let mut year = now_year.div_euclid(100) * 100 + digits(short_year, 2)?;
            if year - now_year > 50 {
                year -= 100;
            }
            (digits(day, 2)?, month_number(month)?, year, *time)
        }
        [weekday, month, day, time, year] if SHORT_DAYS.contains(weekday) => {
            let day_length = if day.len() == 1 { 1 } else { 2 };
            (
                digits(day, day_length)?,
                month_number(month)?,
                digits(year, 4)?,
                *time,
            )
        }
        _ => return None,
    };
    let mut clock = time.split(':');
    let (hour, minute, second) = (
        digits(clock.next()?, 2)?,
        digits(clock.next()?, 2)?,
        digits(clock.next()?, 2)?,
    );
    if clock.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if civil_from_days(days) != (year, month, day) {
        return None;
    }
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date (year, month, day) of a number of days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
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
    raw_json: Value,
    secrets: &ProviderSecrets<'_>,
) -> Result<VehicleData, OwnerApiError> {
    VehicleData::from_provider_raw_json(vehicle_id, raw_json, secrets)
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

    pub(crate) fn try_from_i64(value: i64) -> Option<Self> {
        u64::try_from(value)
            .ok()
            .filter(|value| *value > 0)
            .map(Self)
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

    pub(crate) fn try_from_i64(value: i64) -> Option<Self> {
        u64::try_from(value)
            .ok()
            .filter(|value| *value > 0)
            .map(Self)
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
    provider_raw_json: Value,
}

/// Explicit vehicle mutations supported by the legacy Owner endpoint. These
/// values never enter the collector scheduler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyVehicleAction {
    Wake,
    ClimateStart,
    ClimateStop,
    ChargeStart,
    ChargeStop,
    SetChargeLimit(u8),
    Lock,
    Unlock,
    FlashLights,
    HonkHorn,
}

impl LegacyVehicleAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wake => "wake",
            Self::ClimateStart => "climate_start",
            Self::ClimateStop => "climate_stop",
            Self::ChargeStart => "charge_start",
            Self::ChargeStop => "charge_stop",
            Self::SetChargeLimit(_) => "set_charge_limit",
            Self::Lock => "lock",
            Self::Unlock => "unlock",
            Self::FlashLights => "flash_lights",
            Self::HonkHorn => "honk_horn",
        }
    }

    const fn endpoint_suffix(self) -> &'static str {
        match self {
            Self::Wake => "wake_up",
            Self::ClimateStart => "command/auto_conditioning_start",
            Self::ClimateStop => "command/auto_conditioning_stop",
            Self::ChargeStart => "command/charge_start",
            Self::ChargeStop => "command/charge_stop",
            Self::SetChargeLimit(_) => "command/set_charge_limit",
            Self::Lock => "command/door_lock",
            Self::Unlock => "command/door_unlock",
            Self::FlashLights => "command/flash_lights",
            Self::HonkHorn => "command/honk_horn",
        }
    }

    fn request_body(self) -> Value {
        match self {
            Self::SetChargeLimit(percent) => json!({"percent": percent}),
            _ => json!({}),
        }
    }

    fn request_body_bytes(self) -> Result<Vec<u8>, OwnerApiError> {
        serde_json::to_vec(&self.request_body()).map_err(|_| OwnerApiError::InvalidCommand)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyVehicleActionResult {
    pub state: Option<String>,
}

impl VehicleData {
    pub fn vehicle_id(&self) -> VehicleId {
        self.vehicle_id
    }

    pub fn fields(&self) -> &Map<String, Value> {
        &self.fields
    }

    pub fn provider_raw_json(&self) -> &Value {
        &self.provider_raw_json
    }

    /// Project one provider response. `secrets` are the live credentials in
    /// scope for it: any kept string that is or contains one is replaced by
    /// null here, before anything can store, cache or serve the data.
    pub(crate) fn from_provider_raw_json(
        vehicle_id: VehicleId,
        raw_json: Value,
        secrets: &ProviderSecrets<'_>,
    ) -> Result<Self, OwnerApiError> {
        let count_is_invalid = raw_json
            .as_object()
            .and_then(|root| root.get("count"))
            .is_some_and(|count| !count.is_null());
        if count_is_invalid {
            return Err(OwnerApiError::InvalidVehicleDataEnvelope);
        }

        let mut fields = allowlisted_provider_response_v1(&raw_json)?;
        let reflected = secrets.scrub_fields(&mut fields);
        if !reflected.is_empty() {
            // Paths are allowlisted provider key names; no value is logged.
            tracing::warn!(
                paths = %reflected.join(", "),
                fields = reflected.len(),
                "provider response reflected a live credential; the affected fields were replaced by null"
            );
        }
        let mut provider_raw_json = json!({"response": fields});
        scrub_sensitive_value(&mut provider_raw_json);
        let fields = provider_raw_json["response"]
            .as_object()
            .cloned()
            .ok_or(OwnerApiError::InvalidVehicleDataEnvelope)?;
        if fields.is_empty() {
            return Err(OwnerApiError::SensitiveDataInResponse);
        }
        Ok(Self {
            vehicle_id,
            fields,
            provider_raw_json,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(vehicle_id: u64, fields: Value) -> Self {
        let fields = fields
            .as_object()
            .expect("test vehicle data must be an object")
            .clone();
        Self {
            vehicle_id: VehicleId(vehicle_id),
            provider_raw_json: serde_json::json!({"response": fields.clone()}),
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
    /// HTTP 403 (C3 R42). `fleet_api_named` is whether the body names the Fleet
    /// API, which is Tesla's way of saying this wire no longer serves the
    /// request; the body itself is never kept.
    #[error("owner API refused the request (HTTP 403)")]
    Forbidden { fleet_api_named: bool },
    /// HTTP 410 (C3 R42): RFC 9110's "no longer available, likely permanent".
    #[error("owner API reports the resource gone (HTTP 410)")]
    Gone,
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
    #[error("owner API vehicle command response is invalid")]
    InvalidCommandResponse,
    #[error("owner API vehicle command is invalid")]
    InvalidCommand,
    #[error("owner API vehicle command was rejected")]
    CommandRejected,
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
struct CommandResponseWire {
    result: bool,
    #[serde(default, rename = "reason")]
    _reason: Option<String>,
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
            settings: ProjectionCarSettings::new_live(),
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

pub(crate) fn allowlisted_provider_response_v1(
    raw_json: &Value,
) -> Result<Map<String, Value>, OwnerApiError> {
    const ROOT_FIELDS: &[&str] = &["id", "vehicle_id", "vin", "display_name", "state"];
    const DRIVE_STATE_FIELDS: &[&str] = &[
        "active_route_destination",
        "active_route_energy_at_arrival",
        "active_route_latitude",
        "active_route_longitude",
        "active_route_miles_to_arrival",
        "active_route_minutes_to_arrival",
        "active_route_traffic_minutes_delay",
        "elevation",
        "est_heading",
        "est_lat",
        "est_lng",
        "gps_as_of",
        "heading",
        "latitude",
        "longitude",
        "native_latitude",
        "native_location_elevation",
        "native_location_supported",
        "native_longitude",
        "native_type",
        "power",
        "shift_state",
        "speed",
        "timestamp",
    ];
    const CHARGE_STATE_FIELDS: &[&str] = &[
        "battery_heater_on",
        "battery_level",
        "battery_range",
        "charge_amps",
        "charge_current_request",
        "charge_current_request_max",
        "charge_enable_request",
        "charge_energy_added",
        "charge_limit_soc",
        "charge_miles_added_ideal",
        "charge_miles_added_rated",
        "charge_port_cold_weather_mode",
        "charge_port_color",
        "charge_port_door_open",
        "charge_port_latch",
        "charge_rate",
        "charger_actual_current",
        "charger_phases",
        "charger_pilot_current",
        "charger_power",
        "charger_voltage",
        "charging_state",
        "conn_charge_cable",
        "est_battery_range",
        "fast_charger_brand",
        "fast_charger_present",
        "fast_charger_type",
        "ideal_battery_range",
        "managed_charging_active",
        "managed_charging_start_time",
        "managed_charging_user_canceled",
        "max_range_charge_counter",
        "minutes_to_full_charge",
        "not_enough_power_to_heat",
        "off_peak_charging_enabled",
        "off_peak_charging_times",
        "off_peak_hours_end_time",
        "preconditioning_enabled",
        "preconditioning_times",
        "scheduled_charging_mode",
        "scheduled_charging_pending",
        "scheduled_charging_start_time",
        "scheduled_departure_time",
        "scheduled_departure_time_minutes",
        "supercharger_session_trip_planner",
        "time_to_full_charge",
        "timestamp",
        "trip_charging",
        "usable_battery_level",
        "user_charge_enable_request",
    ];
    const CLIMATE_STATE_FIELDS: &[&str] = &[
        "auto_seat_climate_left",
        "auto_seat_climate_right",
        "battery_heater",
        "battery_heater_no_power",
        "cabin_overheat_protection",
        "cabin_overheat_protection_actively_cooling",
        "climate_keeper_mode",
        "defrost_mode",
        "driver_temp_setting",
        "fan_status",
        "inside_temp",
        "is_auto_conditioning_on",
        "is_climate_on",
        "is_front_defroster_on",
        "is_preconditioning",
        "is_rear_defroster_on",
        "left_temp_direction",
        "max_avail_temp",
        "min_avail_temp",
        "outside_temp",
        "passenger_temp_setting",
        "remote_heater_control_enabled",
        "right_temp_direction",
        "seat_heater_left",
        "seat_heater_rear_center",
        "seat_heater_rear_left",
        "seat_heater_rear_right",
        "seat_heater_right",
        "side_mirror_heaters",
        "steering_wheel_heater",
        "supports_fan_only_cabin_overheat_protection",
        "timestamp",
        "wiper_blade_heater",
    ];
    const VEHICLE_STATE_FIELDS: &[&str] = &[
        "api_version",
        "autopark_state_v2",
        "autopark_style",
        "calendar_supported",
        "car_version",
        "center_display_state",
        "dashcam_clip_save_available",
        "dashcam_state",
        "df",
        "dr",
        "fd_window",
        "fp_window",
        "ft",
        "homelink_device_count",
        "homelink_nearby",
        "is_user_present",
        "last_autopark_error",
        "locked",
        "notifications_supported",
        "odometer",
        "parsed_calendar_supported",
        "pf",
        "pr",
        "rd_window",
        "remote_start",
        "remote_start_enabled",
        "remote_start_supported",
        "rp_window",
        "rt",
        "sentry_mode",
        "sentry_mode_available",
        "service_mode",
        "smart_summon_available",
        "summon_standby_mode_enabled",
        "sun_roof_percent_open",
        "sun_roof_state",
        "timestamp",
        "tpms_hard_warning_fl",
        "tpms_hard_warning_fr",
        "tpms_hard_warning_rl",
        "tpms_hard_warning_rr",
        "tpms_last_seen_pressure_time_fl",
        "tpms_last_seen_pressure_time_fr",
        "tpms_last_seen_pressure_time_rl",
        "tpms_last_seen_pressure_time_rr",
        "tpms_pressure_fl",
        "tpms_pressure_fr",
        "tpms_pressure_rl",
        "tpms_pressure_rr",
        "tpms_rcp_front_value",
        "tpms_rcp_rear_value",
        "tpms_soft_warning_fl",
        "tpms_soft_warning_fr",
        "tpms_soft_warning_rl",
        "tpms_soft_warning_rr",
        "valet_mode",
        "valet_pin_needed",
        "vehicle_name",
    ];
    const SOFTWARE_UPDATE_FIELDS: &[&str] = &["status", "version", "download_perc", "install_perc"];
    const VEHICLE_CONFIG_FIELDS: &[&str] = &[
        "aux_park_lamps",
        "badge_version",
        "can_accept_navigation_requests",
        "can_actuate_trunks",
        "car_special_type",
        "car_type",
        "charge_port_type",
        "cop_user_set_temp_supported",
        "dashcam_clip_save_supported",
        "default_charge_to_max",
        "driver_assist",
        "ece_restrictions",
        "efficiency_package",
        "eu_vehicle",
        "exterior_color",
        "exterior_trim",
        "has_air_suspension",
        "has_ludicrous_mode",
        "has_seat_cooling",
        "headlamp_type",
        "interior_trim_type",
        "key_version",
        "motorized_charge_port",
        "plg",
        "pws",
        "rear_drive_unit",
        "rear_seat_heaters",
        "rear_seat_type",
        "rhd",
        "roof_color",
        "seat_type",
        "spoiler_type",
        "sun_roof_installed",
        "supports_qr_pairing",
        "third_row_seats",
        "timestamp",
        "trim_badging",
        "use_range_badging",
        "utc_offset",
        "webcam_selfie_supported",
        "webcam_supported",
        "wheel_type",
    ];
    const GUI_SETTINGS_FIELDS: &[&str] = &[
        "gui_24_hour_time",
        "gui_charge_rate_units",
        "gui_distance_units",
        "gui_range_display",
        "gui_temperature_units",
        "show_range_units",
        "timestamp",
    ];

    let response = raw_json
        .as_object()
        .and_then(|root| root.get("response"))
        .and_then(Value::as_object)
        .ok_or(OwnerApiError::InvalidVehicleDataEnvelope)?;
    let mut filtered = allowlisted_provider_scalars(response, ROOT_FIELDS);

    for (group_name, allowed_fields) in [
        ("drive_state", DRIVE_STATE_FIELDS),
        ("charge_state", CHARGE_STATE_FIELDS),
        ("climate_state", CLIMATE_STATE_FIELDS),
        ("vehicle_config", VEHICLE_CONFIG_FIELDS),
        ("gui_settings", GUI_SETTINGS_FIELDS),
    ] {
        let Some(group) = response.get(group_name).and_then(Value::as_object) else {
            continue;
        };
        let group = allowlisted_provider_scalars(group, allowed_fields);
        if !group.is_empty() {
            filtered.insert(group_name.to_owned(), Value::Object(group));
        }
    }

    if let Some(vehicle_state) = response.get("vehicle_state").and_then(Value::as_object) {
        let mut vehicle_state = allowlisted_provider_scalars(vehicle_state, VEHICLE_STATE_FIELDS);
        if let Some(update) = response
            .get("vehicle_state")
            .and_then(Value::as_object)
            .and_then(|state| state.get("software_update"))
            .and_then(Value::as_object)
        {
            let update = allowlisted_provider_scalars(update, SOFTWARE_UPDATE_FIELDS);
            if !update.is_empty() {
                vehicle_state.insert("software_update".to_owned(), Value::Object(update));
            }
        }
        if !vehicle_state.is_empty() {
            filtered.insert("vehicle_state".to_owned(), Value::Object(vehicle_state));
        }
    }

    if filtered.is_empty() {
        return Err(OwnerApiError::SensitiveDataInResponse);
    }
    Ok(filtered)
}

fn allowlisted_provider_scalars(
    source: &Map<String, Value>,
    allowed_fields: &[&str],
) -> Map<String, Value> {
    allowed_fields
        .iter()
        .filter_map(|field| {
            source
                .get(*field)
                .and_then(allowlisted_provider_scalar)
                .map(|value| ((*field).to_owned(), value))
        })
        .collect()
}

fn allowlisted_provider_scalar(value: &Value) -> Option<Value> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Some(value.clone()),
        Value::String(text) if text.len() <= MAX_PROVIDER_TELEMETRY_TEXT_BYTES => {
            Some(value.clone())
        }
        Value::String(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

fn scrub_sensitive_fields(fields: &mut Map<String, Value>) {
    fields.retain(|key, value| {
        let normalized = key
            .bytes()
            .filter(u8::is_ascii_alphanumeric)
            .map(|byte| byte.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let normalized = std::str::from_utf8(&normalized).unwrap_or_default();
        let sensitive = normalized == "authorization"
            || normalized.ends_with("authorization")
            || normalized == "tokens"
            || normalized.ends_with("token")
            || normalized == "password"
            || normalized.ends_with("password")
            || normalized == "apikey"
            || normalized.ends_with("apikey")
            || normalized == "cookie"
            || normalized.ends_with("cookie")
            || normalized == "cookies"
            || normalized.ends_with("cookies")
            || normalized == "secret"
            || normalized.ends_with("secret");
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

/// A request that produced no HTTP status: a timeout before a response is
/// `RequestTimeout`; DNS, connect, TLS and every other transport fault is
/// `Transport` (C3 R42, R43). The scheduler treats both as "Tesla not
/// reachable", never as a fault of one car.
fn classify_transport_error(error: reqwest::Error) -> OwnerApiError {
    if error.is_timeout() {
        OwnerApiError::RequestTimeout
    } else {
        OwnerApiError::Transport
    }
}

/// Whether a 405 body says the car is in service (C3 R42, R52).
pub(crate) fn is_vehicle_in_service_body(bytes: &[u8]) -> bool {
    is_owner_error_body(bytes, IN_SERVICE_TEXT)
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
        if !base_url.is_loopback_http() {
            return Err(OwnerApiConfigError::HttpsRequired);
        }
        Self::build(OwnerApiOptions::new(base_url, request_timeout), true)
    }
}

#[cfg(test)]
#[path = "owner_api/tests.rs"]
mod tests;
