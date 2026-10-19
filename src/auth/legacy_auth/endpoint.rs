// SPDX-License-Identifier: AGPL-3.0-only

//! Issuer, region and token-endpoint selection (C5 R1 to R9, R13, R46 for the
//! token grammar of stored pairs).
//!
//! Everything here is local and decides before any network use which host a
//! refresh token may be sent to, or refuses (R5). The JWT is read only to pick
//! between two pinned Tesla hosts, so its signature is never checked (R3).

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use url::{Host, Url};
use zeroize::Zeroizing;

use super::LegacyAuthError;
use crate::{config::CollectorMode, tesla_stream::StreamRegion};

/// The global Tesla issuer (R2, F1). Compared as an exact string (F9).
pub(super) const GLOBAL_ISSUER: &str = "https://auth.tesla.com/oauth2/v3";

/// The China Tesla issuer (R2, F2). Compared as an exact string (F9).
pub(super) const CHINA_ISSUER: &str = "https://auth.tesla.cn/oauth2/v3";

/// The global token endpoint (R6, F1), written out in full because joining
/// `token` onto the issuer without a trailing slash would drop `v3`.
const GLOBAL_TOKEN_ENDPOINT: &str = "https://auth.tesla.com/oauth2/v3/token";

/// The China token endpoint (R6, F2), written out in full for the same reason.
const CHINA_TOKEN_ENDPOINT: &str = "https://auth.tesla.cn/oauth2/v3/token";

/// The last representable RFC 3339 second, 9999-12-31T23:59:59Z (R3, F17). An
/// `exp` above it is treated as absent, which keeps every later `exp - 300`
/// and comparison free of overflow.
pub(super) const MAX_EXP_SECONDS: i64 = 253_402_300_799;

/// Which kind of login the tokens come from (C5 R1). The mode is fixed at
/// construction and applies the issuer pin only to Tesla.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    /// Tokens from Tesla's own login, refreshed only at a pinned Tesla issuer.
    Tesla,
    /// An Owner-API-compatible provider: the token URL is configuration, and
    /// no issuer pin applies (R8).
    Provider,
}

/// Where the token endpoint came from, for diagnostics only (C5 section 3.1).
/// It names no URL and no token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointSource {
    /// Provider mode: `[collector.legacy_auth] token_url` (R8).
    ProviderConfig,
    /// Tesla mode: the pinned issuer named by the access token's `iss` (R4).
    JwtIssuer,
    /// Tesla mode: the configured region, for a token with no usable `iss`.
    ConfiguredRegion,
    /// Test constructors only: the issuer URL a test supplies (section 3.4).
    #[cfg(test)]
    TestOverride,
}

impl EndpointSource {
    /// A fixed word for diagnostics (R53 allows the endpoint source).
    pub const fn code(self) -> &'static str {
        match self {
            Self::ProviderConfig => "provider_config",
            Self::JwtIssuer => "jwt_issuer",
            Self::ConfiguredRegion => "configured_region",
            #[cfg(test)]
            Self::TestOverride => "test_override",
        }
    }
}

/// The configuration inputs of the auth unit (C5 section 3.4), built once from
/// the Hub configuration and passed to every constructor of a `LegacyAuth`.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthEndpointConfig {
    /// Tesla or Provider, from the one mode derivation the collector uses (R1).
    pub mode: AuthMode,
    /// `[collector] stream_region`, only when the key is present in the file
    /// (K5): a present key is the owner's statement of a region (R4).
    pub region: Option<StreamRegion>,
    /// `[collector.legacy_auth] token_url`. Provider mode only (R7).
    pub token_url: Option<String>,
    /// The runtime `allow_insecure_test_base` flag of `OwnerApi::build`, which
    /// only test harnesses set. It alone lets a loopback `http` token URL pass
    /// (R7).
    pub allow_loopback_http: bool,
}

impl AuthEndpointConfig {
    /// Build the configuration from the Hub configuration (C5 section 3.4).
    ///
    /// The mode is the collector's own derivation (C3 R1) so the two units
    /// cannot disagree about whether the issuer pin applies. The auth unit is
    /// never constructed in Fleet polling mode; if it ever were, the stricter
    /// Tesla pin is the safe reading, because the pin is the one rule that
    /// keeps a refresh token from a host the owner did not name (R7).
    pub fn from_config(config: &crate::config::HubConfig, allow_loopback_http: bool) -> Self {
        let collector = &config.collector;
        let mode = match crate::config::collector_mode(collector) {
            CollectorMode::Provider => AuthMode::Provider,
            CollectorMode::Legacy | CollectorMode::FleetPolling => AuthMode::Tesla,
        };
        Self {
            mode,
            region: collector.stream_region,
            token_url: collector.legacy_auth.token_url.clone(),
            allow_loopback_http,
        }
    }
}

impl fmt::Debug for AuthEndpointConfig {
    /// The `token_url` path may carry a provider's route and is never shown
    /// (R53); only whether it is set and its host appear.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let token_url_host = self
            .token_url
            .as_deref()
            .map(|value| match Url::parse(value) {
                Ok(url) => url.host_str().unwrap_or("[no host]").to_owned(),
                Err(_) => "[unparsable]".to_owned(),
            });
        formatter
            .debug_struct("AuthEndpointConfig")
            .field("mode", &self.mode)
            .field("region", &self.region)
            .field("token_url_host", &token_url_host)
            .field("allow_loopback_http", &self.allow_loopback_http)
            .finish()
    }
}

/// The outcome of selecting an endpoint for one stored pair.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct EndpointSelection {
    /// The exact URL the refresh is POSTed to. `None` only for an
    /// access-token-only provider pair (R8a).
    pub token_endpoint: Option<Url>,
    /// The data and stream region (R13).
    pub region: StreamRegion,
    /// Where the endpoint came from.
    pub source: EndpointSource,
    /// The issuer a refreshed JWT access token must carry (R9). `None` in
    /// provider mode, where `iss` is neither checked nor used (R8).
    pub pinned_issuer: Option<String>,
}

/// The data region of a pinned issuer (R2).
fn region_of_issuer(issuer: &str) -> Option<StreamRegion> {
    match issuer {
        GLOBAL_ISSUER => Some(StreamRegion::Global),
        CHINA_ISSUER => Some(StreamRegion::China),
        _ => None,
    }
}

/// The pinned issuer of a region (R2).
pub(super) const fn issuer_of_region(region: StreamRegion) -> &'static str {
    match region {
        StreamRegion::Global => GLOBAL_ISSUER,
        StreamRegion::China => CHINA_ISSUER,
    }
}

/// The token endpoint of a region (R6).
fn token_endpoint_of_region(region: StreamRegion) -> Url {
    let text = match region {
        StreamRegion::Global => GLOBAL_TOKEN_ENDPOINT,
        StreamRegion::China => CHINA_TOKEN_ENDPOINT,
    };
    Url::parse(text).expect("pinned token endpoints are valid URLs")
}

/// The token grammar of R46 for one pair under `mode` (already trimmed): the
/// access token fits `b64token`; Tesla mode needs a refresh token of VSCHARs, and
/// Provider mode accepts an empty one (R8a, access-token-only) or one of VSCHARs.
/// A pair that fails it cannot be used under any configuration. Shared by the
/// endpoint selection and by the read-only effective status, so both judge a
/// stored pair alike.
pub(super) fn check_token_grammar(
    access_token: &str,
    refresh_token: &str,
    mode: AuthMode,
) -> Result<(), LegacyAuthError> {
    if !is_access_token(access_token) {
        return Err(LegacyAuthError::InvalidAccessToken);
    }
    let refresh_ok = match mode {
        AuthMode::Tesla => is_refresh_token(refresh_token),
        AuthMode::Provider => refresh_token.is_empty() || is_refresh_token(refresh_token),
    };
    if refresh_ok {
        Ok(())
    } else {
        Err(LegacyAuthError::InvalidRefreshToken)
    }
}

/// Choose the token endpoint for one pair (C5 R4 to R8a), or refuse before any
/// network use (R5).
///
/// Tokens are checked first (R46), because a stored pair that fails the
/// grammar cannot be used under any configuration, then the configuration.
pub(super) fn select_endpoint(
    access_token: &str,
    refresh_token: &str,
    config: &AuthEndpointConfig,
) -> Result<EndpointSelection, LegacyAuthError> {
    check_token_grammar(access_token, refresh_token, config.mode)?;
    match config.mode {
        AuthMode::Tesla => select_tesla(access_token, config),
        AuthMode::Provider => {
            if refresh_token.is_empty() {
                // R8a: access-token-only. No token request is ever sent, so no
                // token URL is needed and none is read.
                return Ok(EndpointSelection {
                    token_endpoint: None,
                    region: config.region.unwrap_or(StreamRegion::Global),
                    source: EndpointSource::ProviderConfig,
                    pinned_issuer: None,
                });
            }
            Ok(EndpointSelection {
                token_endpoint: Some(validate_token_url(
                    config.token_url.as_deref(),
                    config.allow_loopback_http,
                )?),
                region: config.region.unwrap_or(StreamRegion::Global),
                source: EndpointSource::ProviderConfig,
                pinned_issuer: None,
            })
        }
    }
}

/// Tesla mode: the issuer pin (R2, R4, R5).
fn select_tesla(
    access_token: &str,
    config: &AuthEndpointConfig,
) -> Result<EndpointSelection, LegacyAuthError> {
    if config.token_url.is_some() {
        // R7: the key must not bypass the pin.
        return Err(LegacyAuthError::TokenUrlNotAllowed);
    }
    let claims = parse_jwt(access_token);
    let (region, source) = match claims.as_ref().and_then(|claims| claims.iss.as_deref()) {
        Some(issuer) => {
            let region = region_of_issuer(issuer).ok_or(LegacyAuthError::InvalidIssuer)?;
            if config.region.is_some_and(|configured| configured != region) {
                return Err(LegacyAuthError::RegionConflict);
            }
            (region, EndpointSource::JwtIssuer)
        }
        None => (
            config.region.ok_or(LegacyAuthError::RegionRequired)?,
            EndpointSource::ConfiguredRegion,
        ),
    };
    Ok(EndpointSelection {
        token_endpoint: Some(token_endpoint_of_region(region)),
        region,
        source,
        pinned_issuer: Some(issuer_of_region(region).to_owned()),
    })
}

/// The test-constructor selection (C5 section 3.4): the issuer URL with one
/// path segment `token` appended, region Global, and R9 checked against the
/// given issuer string. `Url::join` is not used (R6).
#[cfg(test)]
pub(super) fn test_override_selection(issuer: &Url) -> EndpointSelection {
    let mut endpoint = issuer.clone();
    let path = format!("{}/token", issuer.path().trim_end_matches('/'));
    endpoint.set_path(&path);
    EndpointSelection {
        token_endpoint: Some(endpoint),
        region: StreamRegion::Global,
        source: EndpointSource::TestOverride,
        pinned_issuer: Some(issuer.as_str().to_owned()),
    }
}

/// Validate a provider `token_url` (C5 R7).
///
/// It must be an absolute `https` URL with a host, without userinfo, query or
/// fragment (F6: TLS and no fragment; no secret can ride in a URL the Hub
/// shows by host). `http` passes only for a loopback IP address and only when
/// the runtime test flag is set.
fn validate_token_url(
    value: Option<&str>,
    allow_loopback_http: bool,
) -> Result<Url, LegacyAuthError> {
    let url = Url::parse(value.ok_or(LegacyAuthError::TokenUrlInvalid)?)
        .map_err(|_| LegacyAuthError::TokenUrlInvalid)?;
    let has_host = url.host_str().is_some_and(|host| !host.is_empty());
    if !has_host
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(LegacyAuthError::TokenUrlInvalid);
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if allow_loopback_http && host_is_loopback_ip(&url) => Ok(url),
        _ => Err(LegacyAuthError::TokenUrlInvalid),
    }
}

/// 127.0.0.0/8 or `::1` (R7). A host name such as `localhost` is not accepted:
/// only an address literal cannot be pointed elsewhere by name resolution.
fn host_is_loopback_ip(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    }
}

/// The claims this unit reads from a JWT access token (C5 R3). Nothing else of
/// the payload is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JwtClaims {
    /// `iss`, only when it is a JSON string.
    pub iss: Option<String>,
    /// `exp` in epoch seconds, only when it lies in `[0, MAX_EXP_SECONDS]`
    /// after its fractional part is dropped.
    pub exp: Option<i64>,
}

/// Recognise a JWT and read `iss` and `exp` (C5 R3). `None` means the token is
/// opaque for this unit: not exactly three `.`-separated segments, an empty or
/// padded or non-base64url header or payload, or one that is not a JSON
/// object. The signature is not verified.
pub(crate) fn parse_jwt(token: &str) -> Option<JwtClaims> {
    let mut segments = token.split('.');
    let header = segments.next()?;
    let payload = segments.next()?;
    let _signature = segments.next()?;
    if segments.next().is_some() || header.is_empty() || payload.is_empty() {
        return None;
    }
    let header = Zeroizing::new(URL_SAFE_NO_PAD.decode(header).ok()?);
    if !serde_json::from_slice::<Value>(&header).ok()?.is_object() {
        return None;
    }
    let payload = Zeroizing::new(URL_SAFE_NO_PAD.decode(payload).ok()?);
    let claims = serde_json::from_slice::<Value>(&payload).ok()?;
    let claims = claims.as_object()?;
    Some(JwtClaims {
        iss: claims.get("iss").and_then(Value::as_str).map(str::to_owned),
        exp: claims.get("exp").and_then(exp_seconds),
    })
}

/// A JWT `exp` as whole seconds, or `None` outside the range of R3.
fn exp_seconds(value: &Value) -> Option<i64> {
    let number = value.as_number()?;
    let seconds = if let Some(unsigned) = number.as_u64() {
        i64::try_from(unsigned).ok()?
    } else if let Some(signed) = number.as_i64() {
        signed
    } else {
        let float = number.as_f64()?.trunc();
        if !(0.0..=MAX_EXP_SECONDS as f64).contains(&float) {
            return None;
        }
        float as i64
    };
    (0..=MAX_EXP_SECONDS).contains(&seconds).then_some(seconds)
}

/// Whether `token` fits the access-token grammar of R46: RFC 6750's `b64token`
/// (F7), one or more of ALPHA, DIGIT and `-._~+/`, with `=` accepted anywhere
/// after at least one such character.
///
/// RFC 6750 allows `=` only as trailing padding. This unit accepts it anywhere
/// because spec test 9 requires a JWT whose payload segment carries padding to
/// load as an opaque token, and such a token has `=` before its last dot. The
/// character is harmless on the wire: what the grammar must rule out is
/// whitespace and control characters, which could split an `Authorization`
/// header, and every other character outside the set stays refused.
pub(crate) fn is_access_token(token: &str) -> bool {
    let all_allowed = token
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte));
    all_allowed && token.bytes().any(|byte| byte != b'=')
}

/// Whether `token` is an RFC 6749 appendix A.17 refresh token (R46, F7): one or
/// more `VSCHAR` (%x20 to %x7E). The refresh token goes into a JSON string, so
/// this keeps control characters out of the request body.
pub(crate) fn is_refresh_token(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

/// Surrounding ASCII whitespace that pasted tokens carry (R46): SP, HTAB, CR
/// and LF. Not the broader Unicode set: the grammar above has no such bytes,
/// so trimming more would only hide an invalid token.
pub(crate) fn trim_token_whitespace(value: &str) -> &str {
    value.trim_matches([' ', '\t', '\r', '\n'])
}
