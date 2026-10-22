// SPDX-License-Identifier: AGPL-3.0-only

//! Reverse-geocoder request building (C7 part A, R4, R6 to R12, R51, R53, R54).
//!
//! These are the pure helpers of the request path: the URL, the header value,
//! the public-host test, the request interval and the `Retry-After` reading.
//! The request itself, admission and the limiter stay in the parent module.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
use url::Url;

use super::{GeocoderError, MIN_REQUEST_INTERVAL};
use crate::{config::ConfigError, location::Wgs84Point};

/// C7 K-PUBLIC-HOST: the one server the public usage policy covers (C7 R51).
const PUBLIC_HOST: &str = "nominatim.openstreetmap.org";
/// C7 K-PUBLIC-INTERVAL: minimum spacing of request starts to the public host (C7 R4).
const PUBLIC_REQUEST_INTERVAL: Duration = Duration::from_secs(15);
/// C7 K-PATH: the service path segment (C7 R6), and its accepted alias.
const REVERSE_SEGMENT: &str = "reverse";
const REVERSE_ALIAS_SEGMENT: &str = "reverse.php";
/// C7 K-FORMAT, K-ZOOM and K-ACCEPT (C7 R8, C7 R10, C7 R13).
const REVERSE_FORMAT: &str = "jsonv2";
const REVERSE_ZOOM: &str = "18";
pub(super) const ACCEPT_JSON: &str = "application/json";

/// C7 R4: the spacing of request starts for an endpoint, 15 seconds for the
/// public host and one second for every other host.
pub(super) fn request_interval(endpoint: &Url) -> Duration {
    if is_public_host(endpoint) {
        PUBLIC_REQUEST_INTERVAL
    } else {
        MIN_REQUEST_INTERVAL
    }
}

/// C7 R51: true exactly when the host, without one trailing dot, is the public
/// host, compared ASCII-case-insensitively; scheme, port and path are ignored.
pub(super) fn is_public_host(endpoint: &Url) -> bool {
    endpoint.host_str().is_some_and(|host| {
        host.strip_suffix('.')
            .unwrap_or(host)
            .eq_ignore_ascii_case(PUBLIC_HOST)
    })
}

/// C7 R6 to R11: the reverse URL for one point. It is pure: the same endpoint,
/// point and language always give the same URL. An endpoint that cannot be a
/// base, or that carries userinfo or a non-empty query, is refused; a fragment
/// and an empty query are dropped.
pub(super) fn reverse_request_url(
    endpoint: &Url,
    point: Wgs84Point,
    language: &str,
) -> Result<Url, GeocoderError> {
    let mut url = endpoint.clone();
    if url.cannot_be_a_base() {
        return Err(GeocoderError::InvalidEndpoint);
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some_and(|query| !query.is_empty())
    {
        return Err(GeocoderError::InvalidEndpoint);
    }
    url.set_fragment(None);
    url.set_query(None);

    let mut segments: Vec<&str> = url
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if !matches!(
        segments.last(),
        Some(&REVERSE_SEGMENT) | Some(&REVERSE_ALIAS_SEGMENT)
    ) {
        segments.push(REVERSE_SEGMENT);
    }
    let mut path = String::new();
    for segment in &segments {
        path.push('/');
        path.push_str(segment);
    }
    url.set_path(&path);

    url.query_pairs_mut()
        .append_pair("format", REVERSE_FORMAT)
        .append_pair("lat", &coordinate_text(point.latitude))
        .append_pair("lon", &coordinate_text(point.longitude))
        .append_pair("zoom", REVERSE_ZOOM)
        .append_pair("addressdetails", "1")
        .append_pair("accept-language", language);
    Ok(url)
}

/// C7 R12: the language as a header value, unaltered. Only visible ASCII is
/// accepted: `HeaderValue::from_str` alone also lets bytes of 128 and above
/// through, which would send text the operator's language list never meant.
pub(super) fn accept_language_value(language: &str) -> Result<HeaderValue, GeocoderError> {
    HeaderValue::from_str(language)
        .ok()
        .filter(|value| value.to_str().is_ok())
        .ok_or(GeocoderError::InvalidConfig(
            ConfigError::InvalidGeocoderLanguage,
        ))
}

/// C7 R9: the shortest text that reads back as the same value, never with an
/// exponent, and `0` for every zero including negative zero.
fn coordinate_text(value: f64) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else {
        value.to_string()
    }
}

/// C7 R53: a failed request, with the provider's requested delay when the
/// status was 403 or 429 and the header was usable (C7 R54).
pub(super) struct RequestFailure {
    pub(super) error: GeocoderError,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "C7 Part C: the back-off reads it")
    )]
    pub(super) retry_after_seconds: Option<u64>,
}

impl From<GeocoderError> for RequestFailure {
    fn from(error: GeocoderError) -> Self {
        Self {
            error,
            retry_after_seconds: None,
        }
    }
}

/// C7 R54: the delay-seconds form of `Retry-After`. Exactly one field whose
/// value, without surrounding spaces and tabs, is one or more ASCII digits gives
/// that number of seconds (`u64::MAX` when it does not fit); anything else,
/// including an HTTP-date, gives `None`.
pub(super) fn retry_after_seconds(headers: &HeaderMap) -> Option<u64> {
    let mut fields = headers.get_all(RETRY_AFTER).iter();
    let field = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    let text = field.to_str().ok()?.trim_matches([' ', '\t']);
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(text.parse::<u64>().unwrap_or(u64::MAX))
}
