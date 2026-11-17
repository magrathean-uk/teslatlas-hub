// SPDX-License-Identifier: AGPL-3.0-only

//! Window-gated registration of the user's own Fleet developer application.

// Kept here to expose the two task-owned library files without changing the
// credential lane's module index. Callers use `partner::keys`.
#[path = "../../auth/fleet_keys.rs"]
pub mod keys;

use super::{
    FleetAccessToken, FleetClientId, FleetRegion,
    egress::{self, PermitKind, Provider},
};
use axum::body::Bytes;
use futures_util::StreamExt;
use reqwest::{Client, Request, Response, redirect::Policy};
use serde::Deserialize;
use std::{fmt, time::Duration};
use url::Url;
use zeroize::Zeroizing;

const MAX_BODY: usize = 16 * 1024;
const MAX_SECRET: usize = 16 * 1024;
const REGISTER_PATH: &str = "/api/1/partner_accounts";
const PUBLIC_KEY_PATH: &str = "/api/1/partner_accounts/public_key";

/// A one-use client secret, never persisted or included in diagnostics.
pub struct PartnerCredentials {
    client_id: FleetClientId,
    secret: Zeroizing<String>,
}

impl PartnerCredentials {
    pub fn new(client_id: FleetClientId, secret: Zeroizing<String>) -> Result<Self, PartnerError> {
        if secret.is_empty()
            || secret.len() > MAX_SECRET
            || secret
                .bytes()
                .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        {
            return Err(PartnerError::InvalidInput);
        }
        Ok(Self { client_id, secret })
    }
}

impl fmt::Debug for PartnerCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PartnerCredentials([redacted])")
    }
}

/// Errors contain only fixed codes, never a provider body, URL or secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PartnerError {
    #[error("fleet_partner_invalid_input")]
    InvalidInput,
    #[error("window_closed")]
    WindowClosed,
    #[error("provider_parked")]
    ProviderParked,
    #[error("fleet_partner_timeout")]
    Timeout,
    #[error("fleet_partner_transport")]
    Transport,
    #[error("fleet_partner_invalid_response")]
    InvalidResponse,
    #[error("fleet_partner_response_1xx")]
    Response1xx,
    #[error("fleet_partner_response_2xx")]
    Response2xx,
    #[error("fleet_partner_response_3xx")]
    Response3xx,
    #[error("fleet_partner_response_4xx")]
    Response4xx,
    #[error("fleet_partner_response_5xx")]
    Response5xx,
    #[error("fleet_partner_auth_failed")]
    Authentication,
    #[error("fleet_partner_public_key_unreachable")]
    PublicKeyUnreachable,
    #[error("fleet_partner_public_key_mismatch")]
    PublicKeyMismatch,
    #[error("fleet_partner_over_limit")]
    OverLimit,
    #[error("fleet_partner_registration_failed")]
    Registration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    Registered,
    AlreadyRegistered,
}

/// Fixed regional endpoints, no redirects, a bounded body and one total
/// timeout per send. There is deliberately no caller-supplied endpoint.
pub struct PartnerApi {
    client: Client,
    timeout: Duration,
    region: FleetRegion,
    auth_url: Url,
    registration_url: Url,
    public_key_url: Url,
    #[cfg(test)]
    stub: Option<super::in_process::InProcessTransport>,
}

impl PartnerApi {
    pub fn new(region: FleetRegion, timeout: Duration) -> Result<Self, PartnerError> {
        if timeout.is_zero() {
            return Err(PartnerError::InvalidInput);
        }
        crate::crypto::install_default_provider();
        let timeout = timeout.min(super::FLEET_TOKEN_REQUEST_MAX_TIMEOUT);
        let builder = Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            .timeout(timeout);
        // Unit tests never initialise the platform trust store or Keychain.
        #[cfg(test)]
        let builder = builder.tls_certs_only(std::iter::empty::<reqwest::Certificate>());
        let client = builder.build().map_err(|_| PartnerError::Transport)?;
        Ok(Self {
            client,
            timeout,
            region,
            auth_url: Url::parse(region.auth_token_url()).expect("fixed Fleet auth URL"),
            registration_url: Url::parse(region.base_url())
                .expect("fixed Fleet URL")
                .join(REGISTER_PATH)
                .expect("fixed partner path"),
            public_key_url: Url::parse(region.base_url())
                .expect("fixed Fleet URL")
                .join(PUBLIC_KEY_PATH)
                .expect("fixed partner key path"),
            #[cfg(test)]
            stub: None,
        })
    }

    /// A client whose every send goes to `transport`, at synthetic hosts.
    #[cfg(test)]
    pub(crate) fn stubbed_for_test(
        region: FleetRegion,
        transport: super::in_process::InProcessTransport,
    ) -> Self {
        let mut api = Self::new(region, Duration::from_secs(1)).expect("test client");
        let id = uuid::Uuid::new_v4().simple().to_string();
        api.auth_url =
            Url::parse(&format!("https://auth-{id}.example.test/oauth2/v3/token")).unwrap();
        api.registration_url =
            Url::parse(&format!("https://fleet-{id}.example.test{REGISTER_PATH}")).unwrap();
        api.public_key_url =
            Url::parse(&format!("https://fleet-{id}.example.test{PUBLIC_KEY_PATH}")).unwrap();
        api.stub = Some(transport);
        api
    }

    /// Obtain a short-lived partner bearer, register the domain, then read
    /// back the key Tesla holds for it and require it to be `expected`, the
    /// Hub's own. Every send asks the Fleet gate independently; no retry or
    /// token is retained.
    pub async fn register(
        &self,
        domain: &str,
        credentials: PartnerCredentials,
        expected: &keys::CommandPublicKey,
    ) -> Result<Registration, PartnerError> {
        let domain = validate_domain(domain)?;
        // Worst-case percent encoding is three bytes per input byte. Allocate
        // before copying the secret; no growing serializer or plain body copy.
        let pairs = [
            ("grant_type", "client_credentials"),
            ("client_id", credentials.client_id.as_str()),
            ("client_secret", credentials.secret.as_str()),
            ("audience", self.region.base_url().trim_end_matches('/')),
            ("scope", "openid vehicle_device_data"),
        ];
        let capacity = pairs
            .iter()
            .map(|(key, value)| 3 * (key.len() + value.len()) + 2)
            .sum();
        let mut form = Zeroizing::new(Vec::with_capacity(capacity));
        for (n, (key, value)) in pairs.iter().enumerate() {
            if n != 0 {
                form.push(b'&');
            }
            for part in url::form_urlencoded::byte_serialize(key.as_bytes()) {
                form.extend_from_slice(part.as_bytes());
            }
            form.push(b'=');
            for part in url::form_urlencoded::byte_serialize(value.as_bytes()) {
                form.extend_from_slice(part.as_bytes());
            }
        }
        let request = self
            .client
            .post(self.auth_url.clone())
            .header("accept", "application/json")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Bytes::from_owner(form))
            .build()
            .map_err(|_| PartnerError::InvalidInput)?;
        drop(credentials);
        let (status, bytes) = self.send(request, PermitKind::Request).await?;
        if !(200..300).contains(&status) {
            return Err(match status {
                400 | 401 => PartnerError::Authentication,
                408 | 500..=599 => PartnerError::Transport,
                429 => PartnerError::OverLimit,
                _ => response_class(status),
            });
        }
        let token = parse_partner_token(&bytes)?;
        drop(bytes);
        let request = self
            .client
            .post(self.registration_url.clone())
            .header("accept", "application/json")
            .header("authorization", bearer_header(&token)?)
            .json(&serde_json::json!({"domain": domain}))
            .build()
            .map_err(|_| PartnerError::InvalidInput)?;
        let (status, bytes) = self.send(request, PermitKind::Request).await?;
        let registration = registration_result(status, &bytes, &domain)?;
        // Tesla fetched the key file at the domain when it registered it (or
        // earlier); what it holds now must be this Hub's key.
        let mut url = self.public_key_url.clone();
        url.query_pairs_mut().append_pair("domain", &domain);
        let request = self
            .client
            .get(url)
            .header("accept", "application/json")
            .header("authorization", bearer_header(&token)?)
            .build()
            .map_err(|_| PartnerError::InvalidInput)?;
        let (status, bytes) = self.send(request, PermitKind::Request).await?;
        registered_key_result(status, &bytes, expected)?;
        Ok(registration)
    }

    async fn send(
        &self,
        request: Request,
        kind: PermitKind,
    ) -> Result<(u16, Zeroizing<Vec<u8>>), PartnerError> {
        // Test clients have only an in-process transport. A missing stub fails
        // closed before any production request can leave a unit test.
        #[cfg(test)]
        if self.stub.is_none() {
            return Err(PartnerError::Transport);
        }
        let permit =
            egress::ask(request.url().as_str(), Provider::Fleet, kind).map_err(|e| match e {
                egress::GateRefusal::WindowClosed => PartnerError::WindowClosed,
            })?;
        permit
            .bound(async {
                tokio::time::timeout(self.timeout, async {
                    #[cfg(test)]
                    let response = self
                        .stub
                        .as_ref()
                        .expect("checked stub")
                        .call(request)
                        .await
                        .map_err(|_| PartnerError::Transport)?;
                    #[cfg(not(test))]
                    let response = self.client.execute(request).await.map_err(|e| {
                        if e.is_timeout() {
                            PartnerError::Timeout
                        } else {
                            PartnerError::Transport
                        }
                    })?;
                    let status = response.status().as_u16();
                    let body = read_body(response).await.map_err(|error| {
                        if error == PartnerError::InvalidResponse {
                            response_class(status)
                        } else {
                            error
                        }
                    })?;
                    Ok((status, body))
                })
                .await
                .map_err(|_| PartnerError::Timeout)?
            })
            .await
            .map_err(|cut| match cut {
                egress::Cut::Closed => PartnerError::WindowClosed,
                egress::Cut::Deadline => PartnerError::Timeout,
            })?
    }
}

// Borrow directly from the wiping response body. Escaped JSON strings are
// refused before serde runs, so its scratch buffer never decodes a secret.
#[derive(Deserialize)]
struct PartnerToken<'a> {
    access_token: &'a str,
    token_type: &'a str,
    expires_in: u64,
}

fn parse_partner_token(bytes: &[u8]) -> Result<FleetAccessToken, PartnerError> {
    if bytes.contains(&b'\\') {
        return Err(PartnerError::Response2xx);
    }
    let reply: PartnerToken<'_> =
        serde_json::from_slice(bytes).map_err(|_| PartnerError::Response2xx)?;
    if !reply.token_type.eq_ignore_ascii_case("Bearer")
        || reply.expires_in == 0
        || reply.expires_in > super::MAX_TOKEN_LIFETIME_SECONDS
    {
        return Err(PartnerError::Response2xx);
    }
    FleetAccessToken::new(reply.access_token.to_owned()).map_err(|_| PartnerError::Response2xx)
}

fn bearer_header(token: &FleetAccessToken) -> Result<http::HeaderValue, PartnerError> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(7 + token.expose().len()));
    bytes.extend_from_slice(b"Bearer ");
    bytes.extend_from_slice(token.expose().as_bytes());
    let mut header = http::HeaderValue::from_maybe_shared(Bytes::from_owner(bytes))
        .map_err(|_| PartnerError::InvalidInput)?;
    header.set_sensitive(true);
    Ok(header)
}

fn response_class(status: u16) -> PartnerError {
    match status {
        100..=199 => PartnerError::Response1xx,
        200..=299 => PartnerError::Response2xx,
        300..=399 => PartnerError::Response3xx,
        400..=499 => PartnerError::Response4xx,
        _ => PartnerError::Response5xx,
    }
}

/// The domain as it is registered: a lowercase host name, no scheme, port,
/// path, trailing dot or IP address.
pub fn validate_domain(domain: &str) -> Result<String, PartnerError> {
    if domain.len() > 253
        || !domain.is_ascii()
        || !domain.contains('.')
        || domain.parse::<std::net::IpAddr>().is_ok()
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err(PartnerError::InvalidInput);
    }
    Ok(domain.to_ascii_lowercase())
}

async fn read_body(response: Response) -> Result<Zeroizing<Vec<u8>>, PartnerError> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(PartnerError::InvalidResponse);
    }
    let mut body = Zeroizing::new(Vec::with_capacity(MAX_BODY));
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            if e.is_timeout() {
                PartnerError::Timeout
            } else {
                PartnerError::Transport
            }
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_BODY {
            return Err(PartnerError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The supported key answer is {"response":{"public_key":"04.."}}.
/// Every other shape fails with its status class. No provider prose or
/// undocumented error code establishes registration or diagnoses a cause.
fn registered_key_result(
    status: u16,
    bytes: &[u8],
    expected: &keys::CommandPublicKey,
) -> Result<(), PartnerError> {
    if !(200..300).contains(&status) {
        return Err(response_class(status));
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| response_class(status))?;
    let held = value
        .get("response")
        .and_then(|response| response.get("public_key"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| response_class(status))?;
    if held.len() != 130 || !held.starts_with("04") || !held.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(response_class(status));
    }
    if !held.eq_ignore_ascii_case(&expected.public_point_hex()) {
        return Err(PartnerError::PublicKeyMismatch);
    }
    Ok(())
}

fn registration_result(
    status: u16,
    bytes: &[u8],
    domain: &str,
) -> Result<Registration, PartnerError> {
    // 429 is an HTTP rate-limit refusal. All other non-success answers remain
    // plain failures: no documented already-registration wire shape is known.
    if status == 429 {
        return Err(PartnerError::OverLimit);
    }
    if !(200..300).contains(&status) {
        return Err(response_class(status));
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| response_class(status))?;
    let registered = value
        .get("response")
        .and_then(|response| response.get("domain"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| response_class(status))?;
    if !registered.eq_ignore_ascii_case(domain) {
        return Err(response_class(status));
    }
    Ok(Registration::Registered)
}

#[cfg(test)]
mod tests {
    use super::super::in_process::{InProcessAnswer, InProcessTransport};
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    const TOKEN: &str =
        r#"{"access_token":"synthetic-partner-bearer","token_type":"Bearer","expires_in":3600}"#;
    const REGISTERED: &str = r#"{"response":{"domain":"synthetic.example.test"}}"#;

    /// The synthetic Hub key's point, 0x04 then X and Y.
    fn point() -> Vec<u8> {
        let mut point = vec![0x04];
        point.extend((1..=64).map(|n: u8| n.wrapping_mul(37)));
        point
    }

    fn hub_key() -> keys::CommandPublicKey {
        keys::CommandPublicKey::synthetic_for_test(point())
    }

    /// Tesla's answer holding `point` for the domain.
    fn held(point: &[u8]) -> String {
        let hex: String = point.iter().map(|byte| format!("{byte:02X}")).collect();
        format!(r#"{{"response":{{"public_key":"{hex}"}}}}"#)
    }

    fn credentials() -> PartnerCredentials {
        PartnerCredentials::new(
            FleetClientId::parse("synthetic-client").unwrap(),
            Zeroizing::new("synthetic-client-secret".to_owned()),
        )
        .unwrap()
    }

    fn answer(status: u16, body: &str) -> InProcessAnswer {
        Ok(http::Response::builder()
            .status(status)
            .body(body.as_bytes().to_vec())
            .unwrap())
    }

    fn api(transport: InProcessTransport) -> PartnerApi {
        PartnerApi::stubbed_for_test(FleetRegion::EuropeMiddleEastAndAfrica, transport)
    }

    fn scripted(
        auth_status: u16,
        auth_body: &str,
        registration_status: u16,
        registration_body: &str,
    ) -> (PartnerApi, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let bodies = [
            auth_body.to_owned(),
            registration_body.to_owned(),
            held(&point()),
        ];
        let statuses = [auth_status, registration_status, 200];
        let api = api(InProcessTransport::new(move |_| {
            let n = count.fetch_add(1, Ordering::SeqCst);
            let result = answer(statuses[n], &bodies[n]);
            async { result }
        }));
        (api, calls)
    }

    #[tokio::test]
    async fn fleet_partner_registers_with_client_credentials_and_regional_audience() {
        for region in [
            FleetRegion::NorthAmericaAndAsiaPacific,
            FleetRegion::EuropeMiddleEastAndAfrica,
            FleetRegion::China,
        ] {
            let count = Arc::new(AtomicUsize::new(0));
            let calls = count.clone();
            let mut client = api(InProcessTransport::new(move |request| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    assert_eq!(request.method(), http::Method::POST);
                }
                let response = if n == 0 {
                    assert!(request.uri().path() == "/oauth2/v3/token");
                    assert!(
                        request.headers().get("content-type").unwrap()
                            == "application/x-www-form-urlencoded"
                    );
                    assert!(request.headers().get("authorization").is_none());
                    let form: std::collections::BTreeMap<_, _> =
                        url::form_urlencoded::parse(request.body()).collect();
                    assert!(form.len() == 5);
                    assert!(form["grant_type"] == "client_credentials");
                    assert!(form["client_id"] == "synthetic-client");
                    assert!(form["client_secret"] == "synthetic-client-secret");
                    assert!(form["audience"] == region.base_url().trim_end_matches('/'));
                    assert!(form["scope"] == "openid vehicle_device_data");
                    answer(200, TOKEN)
                } else if n == 2 {
                    assert_eq!(request.method(), http::Method::GET);
                    assert!(request.uri().path() == PUBLIC_KEY_PATH);
                    assert!(request.uri().query() == Some("domain=synthetic.example.test"));
                    assert!(
                        request.headers().get("authorization").unwrap()
                            == "Bearer synthetic-partner-bearer"
                    );
                    assert!(request.body().is_empty());
                    answer(200, &held(&point()))
                } else {
                    assert!(request.uri().path() == REGISTER_PATH);
                    assert!(
                        request.headers().get("authorization").unwrap()
                            == "Bearer synthetic-partner-bearer"
                    );
                    let value: serde_json::Value = serde_json::from_slice(request.body()).unwrap();
                    assert!(value == serde_json::json!({"domain":"synthetic.example.test"}));
                    assert!(
                        !String::from_utf8_lossy(request.body())
                            .contains("synthetic-client-secret")
                    );
                    answer(200, REGISTERED)
                };
                async { response }
            }));
            client.region = region;
            assert_eq!(
                client
                    .register("SYNTHETIC.EXAMPLE.TEST", credentials(), &hub_key())
                    .await,
                Ok(Registration::Registered)
            );
            assert_eq!(count.load(Ordering::SeqCst), 3);
        }
    }

    #[tokio::test]
    async fn fleet_partner_never_guesses_success_or_cause_from_provider_prose() {
        for (status, body, expected) in [
            (
                409,
                r#"{"error":"already_registered"}"#,
                PartnerError::Response4xx,
            ),
            (
                400,
                r#"{"error_description":"Domain already registered to another account"}"#,
                PartnerError::Response4xx,
            ),
            (
                422,
                r#"{"error":"public_key_not_found"}"#,
                PartnerError::Response4xx,
            ),
            (
                400,
                r#"{"error":"limit_exceeded"}"#,
                PartnerError::Response4xx,
            ),
            (
                402,
                "unrecognised billing answer",
                PartnerError::Response4xx,
            ),
            (401, "unrecognised refusal", PartnerError::Response4xx),
            (
                429,
                "unrecognised rate-limit answer",
                PartnerError::OverLimit,
            ),
            (503, "unrecognised failure", PartnerError::Response5xx),
            (302, "redirect", PartnerError::Response3xx),
            (200, "unknown success shape", PartnerError::Response2xx),
        ] {
            let (client, count) = scripted(200, TOKEN, status, body);
            assert_eq!(
                client
                    .register("synthetic.example.test", credentials(), &hub_key())
                    .await,
                Err(expected)
            );
            assert_eq!(count.load(Ordering::SeqCst), 2);
        }
    }

    #[test]
    fn fleet_partner_regional_endpoints_are_the_production_addresses() {
        for (region, base, auth) in [
            (
                FleetRegion::NorthAmericaAndAsiaPacific,
                "https://fleet-api.prd.na.vn.cloud.tesla.com",
                "https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token",
            ),
            (
                FleetRegion::EuropeMiddleEastAndAfrica,
                "https://fleet-api.prd.eu.vn.cloud.tesla.com",
                "https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token",
            ),
            (
                FleetRegion::China,
                "https://fleet-api.prd.cn.vn.cloud.tesla.cn",
                "https://auth.tesla.cn/oauth2/v3/token",
            ),
        ] {
            let api = PartnerApi::new(region, Duration::from_secs(1)).unwrap();
            assert_eq!(api.auth_url.as_str(), auth);
            assert_eq!(
                api.registration_url.as_str(),
                format!("{base}{REGISTER_PATH}")
            );
            assert_eq!(
                api.public_key_url.as_str(),
                format!("{base}{PUBLIC_KEY_PATH}")
            );
        }
    }

    #[test]
    fn fleet_partner_real_secret_holders_and_header_debug_are_redacted() {
        let credentials = credentials();
        assert_eq!(format!("{credentials:?}"), "PartnerCredentials([redacted])");
        assert!(!format!("{credentials:?}").contains(credentials.secret.as_str()));
        let token = parse_partner_token(TOKEN.as_bytes()).unwrap();
        assert!(!format!("{token:?}").contains(token.expose()));
        let header = bearer_header(&token).unwrap();
        assert!(header.is_sensitive());
        assert!(!format!("{header:?}").contains(token.expose()));
        assert_eq!(header.as_bytes(), b"Bearer synthetic-partner-bearer");
    }

    #[tokio::test]
    async fn fleet_partner_auth_failures_never_register() {
        for (status, body, expected) in [
            (
                401,
                r#"{"error":"synthetic-sensitive-echo"}"#,
                PartnerError::Authentication,
            ),
            (429, "synthetic-sensitive-echo", PartnerError::OverLimit),
            (400, "bad request", PartnerError::Authentication),
            (408, "request timeout", PartnerError::Transport),
            (500, "internal error", PartnerError::Transport),
            (503, "unavailable", PartnerError::Transport),
            (403, "unknown forbidden answer", PartnerError::Response4xx),
            (200, "not-json", PartnerError::Response2xx),
            (
                200,
                r#"{"access_token":"synthetic","token_type":"Basic","expires_in":30}"#,
                PartnerError::Response2xx,
            ),
            (
                200,
                r#"{"access_token":"synthetic","token_type":"Bearer","expires_in":0}"#,
                PartnerError::Response2xx,
            ),
            (
                200,
                r#"{"access_token":"synthetic\ninvalid","token_type":"Bearer","expires_in":30}"#,
                PartnerError::Response2xx,
            ),
            (
                200,
                r#"{"access_token":"synthetic"}"#,
                PartnerError::Response2xx,
            ),
        ] {
            let (client, count) = scripted(status, body, 200, REGISTERED);
            assert_eq!(
                client
                    .register("synthetic.example.test", credentials(), &hub_key())
                    .await,
                Err(expected)
            );
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn fleet_partner_refuses_invalid_inputs_before_sending() {
        let (client, count) = scripted(200, TOKEN, 200, REGISTERED);
        for domain in [
            "",
            "https://synthetic.example.test",
            "127.0.0.1",
            "-synthetic.example.test",
            "synthetic.example.test/",
            "synthetic..test",
            "synthetic.example.test:443",
            "synthetic.example.test.",
        ] {
            assert_eq!(
                client.register(domain, credentials(), &hub_key()).await,
                Err(PartnerError::InvalidInput)
            );
        }
        assert_eq!(count.load(Ordering::SeqCst), 0);
        for secret in [
            "".to_owned(),
            "synthetic\nsecret".to_owned(),
            "s".repeat(MAX_SECRET + 1),
        ] {
            assert!(
                PartnerCredentials::new(
                    FleetClientId::parse("synthetic").unwrap(),
                    Zeroizing::new(secret)
                )
                .is_err()
            );
        }
        assert!(!format!("{:?}", credentials()).contains("synthetic-client-secret"));
        assert!(PartnerApi::new(FleetRegion::China, Duration::ZERO).is_err());
    }

    #[tokio::test]
    async fn fleet_partner_refuses_malformed_oversized_and_redirect_responses() {
        for (status, body) in [
            (200, "not-json"),
            (200, "{}"),
            (200, r#"{"response":{"domain":"another.example.test"}}"#),
            (302, r#"{"error":"redirect"}"#),
        ] {
            let (client, count) = scripted(200, TOKEN, status, body);
            assert!(
                client
                    .register("synthetic.example.test", credentials(), &hub_key())
                    .await
                    .is_err()
            );
            assert_eq!(count.load(Ordering::SeqCst), 2);
        }
        for auth in [true, false] {
            let body = "x".repeat(MAX_BODY + 1);
            let (client, count) = if auth {
                scripted(200, &body, 200, REGISTERED)
            } else {
                scripted(200, TOKEN, 200, &body)
            };
            assert_eq!(
                client
                    .register("synthetic.example.test", credentials(), &hub_key())
                    .await,
                Err(PartnerError::Response2xx)
            );
            assert_eq!(count.load(Ordering::SeqCst), if auth { 1 } else { 2 });
        }
    }

    #[tokio::test]
    async fn fleet_partner_closed_gate_prevents_auth_send() {
        let (client, count) = scripted(200, TOKEN, 200, REGISTERED);
        let window = egress::test_support::TestWindow::closed();
        let _route = window.route(client.auth_url.as_str());
        assert_eq!(
            client
                .register("synthetic.example.test", credentials(), &hub_key())
                .await,
            Err(PartnerError::WindowClosed)
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn fleet_partner_asks_gate_again_before_registration() {
        let (client, count) = scripted(200, TOKEN, 200, REGISTERED);
        let window = egress::test_support::TestWindow::closed();
        let _route = window.route(client.registration_url.as_str());
        assert_eq!(
            client
                .register("synthetic.example.test", credentials(), &hub_key())
                .await,
            Err(PartnerError::WindowClosed)
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fleet_partner_transport_failure_is_redacted_and_not_retried() {
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let client = api(InProcessTransport::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(super::super::FleetApiError::Transport) }
        }));
        assert_eq!(
            client
                .register("synthetic.example.test", credentials(), &hub_key())
                .await,
            Err(PartnerError::Transport)
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fleet_partner_production_client_cannot_send_in_a_unit_test() {
        let client = PartnerApi::new(FleetRegion::China, Duration::from_secs(1)).unwrap();
        assert_eq!(
            client
                .register("synthetic.example.test", credentials(), &hub_key())
                .await,
            Err(PartnerError::Transport)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fleet_partner_times_out_an_unanswered_auth_registration_or_key_read() {
        for stall_at in [0, 1, 2] {
            let count = Arc::new(AtomicUsize::new(0));
            let calls = count.clone();
            let client = api(InProcessTransport::new(move |_| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == stall_at {
                        std::future::pending().await
                    } else if n == 0 {
                        answer(200, TOKEN)
                    } else {
                        answer(200, REGISTERED)
                    }
                }
            }));
            assert_eq!(
                client
                    .register("synthetic.example.test", credentials(), &hub_key())
                    .await,
                Err(PartnerError::Timeout)
            );
            assert_eq!(count.load(Ordering::SeqCst), stall_at + 1);
        }
    }

    #[tokio::test]
    async fn fleet_partner_requires_the_registered_key_to_be_the_hubs() {
        let mut other = point();
        other[64] ^= 1;
        for (status, body, expected) in [
            (200, held(&point()), Ok(Registration::Registered)),
            (
                200,
                held(&point()).to_ascii_lowercase(),
                Ok(Registration::Registered),
            ),
            (200, held(&other), Err(PartnerError::PublicKeyMismatch)),
            (
                404,
                "unknown not-found shape".to_owned(),
                Err(PartnerError::Response4xx),
            ),
            (
                200,
                r#"{"response":{"public_key":"04"}}"#.to_owned(),
                Err(PartnerError::Response2xx),
            ),
            (
                200,
                r#"{"response":{}}"#.to_owned(),
                Err(PartnerError::Response2xx),
            ),
            (
                401,
                "unknown auth answer".to_owned(),
                Err(PartnerError::Response4xx),
            ),
            (
                500,
                "unknown failure".to_owned(),
                Err(PartnerError::Response5xx),
            ),
        ] {
            let count = Arc::new(AtomicUsize::new(0));
            let calls = count.clone();
            let client = api(InProcessTransport::new(move |_| {
                let result = match calls.fetch_add(1, Ordering::SeqCst) {
                    0 => answer(200, TOKEN),
                    1 => answer(200, REGISTERED),
                    _ => answer(status, &body),
                };
                async { result }
            }));
            assert_eq!(
                client
                    .register("synthetic.example.test", credentials(), &hub_key())
                    .await,
                expected
            );
            assert_eq!(count.load(Ordering::SeqCst), 3);
        }
    }

    #[tokio::test]
    async fn fleet_partner_window_close_cuts_each_in_flight_send_without_grace() {
        for stall_at in 0..3 {
            let window = Arc::new(egress::test_support::TestWindow::open(60));
            let close = window.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let client = api(InProcessTransport::new(move |_| {
                let n = count.fetch_add(1, Ordering::SeqCst);
                let close = close.clone();
                async move {
                    if n == stall_at {
                        close.close();
                        std::future::pending().await
                    } else if n == 0 {
                        answer(200, TOKEN)
                    } else {
                        answer(200, REGISTERED)
                    }
                }
            }));
            let _auth = window.route(client.auth_url.as_str());
            let _fleet = window.route(client.registration_url.as_str());
            assert_eq!(
                client
                    .register("synthetic.example.test", credentials(), &hub_key())
                    .await,
                Err(PartnerError::WindowClosed)
            );
            assert_eq!(calls.load(Ordering::SeqCst), stall_at + 1);
        }
    }

    #[tokio::test]
    async fn fleet_partner_window_close_between_sends_prevents_next_send() {
        for close_after in 0..2 {
            let window = Arc::new(egress::test_support::TestWindow::open(60));
            let close = window.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let client = api(InProcessTransport::new(move |_| {
                let n = count.fetch_add(1, Ordering::SeqCst);
                if n == close_after {
                    close.close();
                }
                let result = if n == 0 {
                    answer(200, TOKEN)
                } else {
                    answer(200, REGISTERED)
                };
                async { result }
            }));
            let _auth = window.route(client.auth_url.as_str());
            let _fleet = window.route(client.registration_url.as_str());
            assert_eq!(
                client
                    .register("synthetic.example.test", credentials(), &hub_key())
                    .await,
                Err(PartnerError::WindowClosed)
            );
            assert_eq!(calls.load(Ordering::SeqCst), close_after + 1);
        }
    }

    #[tokio::test]
    async fn fleet_partner_asks_gate_again_before_the_key_read() {
        let (mut client, count) = scripted(200, TOKEN, 200, REGISTERED);
        // The test gate routes by host and port: give the key read its own.
        client.public_key_url = Url::parse(&format!(
            "https://key-{}.example.test{PUBLIC_KEY_PATH}",
            uuid::Uuid::new_v4().simple()
        ))
        .unwrap();
        let window = egress::test_support::TestWindow::closed();
        let _route = window.route(client.public_key_url.as_str());
        assert_eq!(
            client
                .register("synthetic.example.test", credentials(), &hub_key())
                .await,
            Err(PartnerError::WindowClosed)
        );
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}
