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
use futures_util::StreamExt;
use reqwest::{Client, Request, Response, redirect::Policy};
use serde::Deserialize;
use std::{fmt, time::Duration};
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const MAX_BODY: usize = 16 * 1024;
const MAX_SECRET: usize = 16 * 1024;
const REGISTER_PATH: &str = "/api/1/partner_accounts";

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
    #[error("fleet_partner_auth_failed")]
    Authentication,
    #[error("fleet_partner_public_key_unreachable")]
    PublicKeyUnreachable,
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
            #[cfg(test)]
            stub: None,
        })
    }

    /// Obtain a short-lived partner bearer, then register the domain. Both
    /// sends ask the Fleet gate independently; no retry or token is retained.
    pub async fn register(
        &self,
        domain: &str,
        credentials: PartnerCredentials,
    ) -> Result<Registration, PartnerError> {
        let domain = validate_domain(domain)?;
        // The URL form encoder must own its output. Keep that allocation
        // wiping-owned; reqwest owns the copy until the send ends, as for the
        // existing Fleet refresh client.
        let form = Zeroizing::new(
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("grant_type", "client_credentials")
                .append_pair("client_id", credentials.client_id.as_str())
                .append_pair("client_secret", &credentials.secret)
                .append_pair("audience", self.region.base_url().trim_end_matches('/'))
                .append_pair("scope", "openid vehicle_device_data")
                .finish(),
        );
        let request = self
            .client
            .post(self.auth_url.clone())
            .header("accept", "application/json")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form.as_bytes().to_vec())
            .build()
            .map_err(|_| PartnerError::InvalidInput)?;
        let (status, bytes) = self.send(request, PermitKind::TokenExchange).await?;
        if !(200..300).contains(&status) {
            return Err(if status == 429 {
                PartnerError::OverLimit
            } else {
                PartnerError::Authentication
            });
        }
        let mut reply: PartnerToken =
            serde_json::from_slice(&bytes).map_err(|_| PartnerError::InvalidResponse)?;
        if !reply.token_type.eq_ignore_ascii_case("Bearer")
            || reply.expires_in == 0
            || reply.expires_in > super::MAX_TOKEN_LIFETIME_SECONDS
        {
            return Err(PartnerError::InvalidResponse);
        }
        let token = FleetAccessToken::new(std::mem::take(&mut reply.access_token))
            .map_err(|_| PartnerError::InvalidResponse)?;
        let request = self
            .client
            .post(self.registration_url.clone())
            .header("accept", "application/json")
            .bearer_auth(token.expose())
            .json(&serde_json::json!({"domain": domain}))
            .build()
            .map_err(|_| PartnerError::InvalidInput)?;
        let (status, bytes) = self.send(request, PermitKind::Request).await?;
        registration_result(status, &bytes, &domain)
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
                    Ok((status, read_body(response).await?))
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

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct PartnerToken {
    access_token: String,
    token_type: String,
    expires_in: u64,
}

fn validate_domain(domain: &str) -> Result<String, PartnerError> {
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
    let mut body = Zeroizing::new(Vec::new());
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

fn registration_result(
    status: u16,
    bytes: &[u8],
    domain: &str,
) -> Result<Registration, PartnerError> {
    if status == 429 || status == 402 {
        return Err(PartnerError::OverLimit);
    }
    if status == 401 || status == 403 {
        return Err(PartnerError::Authentication);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| PartnerError::InvalidResponse)?;
    if (200..300).contains(&status) {
        let response = value
            .get("response")
            .and_then(serde_json::Value::as_object)
            .ok_or(PartnerError::InvalidResponse)?;
        if response
            .get("domain")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|registered| !registered.eq_ignore_ascii_case(domain))
        {
            return Err(PartnerError::InvalidResponse);
        }
        return Ok(Registration::Registered);
    }
    // Only recognise fixed provider codes or documented message fragments;
    // never carry uncontrolled provider text into an error or its Debug.
    let error = value
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let description = value
        .get("error_description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let combined = format!("{error} {description}").to_ascii_lowercase();
    if matches!(status, 400 | 409 | 422)
        && (matches!(
            error,
            "already_registered" | "domain_already_registered" | "partner_account_already_exists"
        ) || combined.contains("domain is already registered")
            || combined.contains("domain already registered"))
    {
        return Ok(Registration::AlreadyRegistered);
    }
    if matches!(error, "public_key_not_found" | "public_key_unreachable")
        || (combined.contains("public key")
            && (combined.contains("not found") || combined.contains("not reachable")))
    {
        return Err(PartnerError::PublicKeyUnreachable);
    }
    if matches!(
        error,
        "rate_limited" | "over_limit" | "limit_exceeded" | "payment_required"
    ) {
        return Err(PartnerError::OverLimit);
    }
    Err(PartnerError::Registration)
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
        let mut api = PartnerApi::new(
            FleetRegion::EuropeMiddleEastAndAfrica,
            Duration::from_secs(1),
        )
        .unwrap();
        let id = uuid::Uuid::new_v4().simple().to_string();
        api.auth_url =
            Url::parse(&format!("https://auth-{id}.example.test/oauth2/v3/token")).unwrap();
        api.registration_url =
            Url::parse(&format!("https://fleet-{id}.example.test{REGISTER_PATH}")).unwrap();
        api.stub = Some(transport);
        api
    }

    fn scripted(
        auth_status: u16,
        auth_body: &str,
        registration_status: u16,
        registration_body: &str,
    ) -> (PartnerApi, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let bodies = [auth_body.to_owned(), registration_body.to_owned()];
        let statuses = [auth_status, registration_status];
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
                assert_eq!(request.method(), http::Method::POST);
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
                    .register("SYNTHETIC.EXAMPLE.TEST", credentials())
                    .await,
                Ok(Registration::Registered)
            );
            assert_eq!(count.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn fleet_partner_accepts_only_explicit_already_registered_errors() {
        for (status, body, expected) in [
            (
                409,
                r#"{"error":"already_registered"}"#,
                Ok(Registration::AlreadyRegistered),
            ),
            (
                400,
                r#"{"error":"invalid_request","error_description":"Domain is already registered"}"#,
                Ok(Registration::AlreadyRegistered),
            ),
            (
                409,
                r#"{"error":"unrelated_conflict"}"#,
                Err(PartnerError::Registration),
            ),
            (
                500,
                r#"{"error":"already_registered"}"#,
                Err(PartnerError::Registration),
            ),
        ] {
            let (client, count) = scripted(200, TOKEN, status, body);
            assert_eq!(
                client
                    .register("synthetic.example.test", credentials())
                    .await,
                expected
            );
            assert_eq!(count.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn fleet_partner_maps_key_limit_auth_and_unknown_errors_without_provider_text() {
        for (status, body, expected) in [
            (
                400,
                r#"{"error":"public_key_not_found","error_description":"synthetic-sensitive-echo"}"#,
                PartnerError::PublicKeyUnreachable,
            ),
            (
                422,
                r#"{"error":"invalid_request","error_description":"Public key not found at synthetic.example.test"}"#,
                PartnerError::PublicKeyUnreachable,
            ),
            (429, "synthetic-sensitive-echo", PartnerError::OverLimit),
            (402, "synthetic-sensitive-echo", PartnerError::OverLimit),
            (
                400,
                r#"{"error":"limit_exceeded"}"#,
                PartnerError::OverLimit,
            ),
            (
                401,
                "synthetic-sensitive-echo",
                PartnerError::Authentication,
            ),
            (
                403,
                "synthetic-sensitive-echo",
                PartnerError::Authentication,
            ),
            (
                503,
                r#"{"error":"synthetic-sensitive-echo"}"#,
                PartnerError::Registration,
            ),
        ] {
            let (client, _) = scripted(200, TOKEN, status, body);
            let error = client
                .register("synthetic.example.test", credentials())
                .await
                .unwrap_err();
            assert_eq!(error, expected);
            assert!(!format!("{error:?} {error}").contains("synthetic-sensitive-echo"));
        }
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
            (200, "not-json", PartnerError::InvalidResponse),
            (
                200,
                r#"{"access_token":"synthetic","token_type":"Basic","expires_in":30}"#,
                PartnerError::InvalidResponse,
            ),
            (
                200,
                r#"{"access_token":"synthetic","token_type":"Bearer","expires_in":0}"#,
                PartnerError::InvalidResponse,
            ),
            (
                200,
                r#"{"access_token":"synthetic\ninvalid","token_type":"Bearer","expires_in":30}"#,
                PartnerError::InvalidResponse,
            ),
            (
                200,
                r#"{"access_token":"synthetic"}"#,
                PartnerError::InvalidResponse,
            ),
        ] {
            let (client, count) = scripted(status, body, 200, REGISTERED);
            assert_eq!(
                client
                    .register("synthetic.example.test", credentials())
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
                client.register(domain, credentials()).await,
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
                    .register("synthetic.example.test", credentials())
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
                    .register("synthetic.example.test", credentials())
                    .await,
                Err(PartnerError::InvalidResponse)
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
                .register("synthetic.example.test", credentials())
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
                .register("synthetic.example.test", credentials())
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
                .register("synthetic.example.test", credentials())
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
                .register("synthetic.example.test", credentials())
                .await,
            Err(PartnerError::Transport)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fleet_partner_times_out_an_unanswered_auth_or_registration() {
        for stall_at in [0, 1] {
            let count = Arc::new(AtomicUsize::new(0));
            let calls = count.clone();
            let client = api(InProcessTransport::new(move |_| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == stall_at {
                        std::future::pending().await
                    } else {
                        answer(200, TOKEN)
                    }
                }
            }));
            assert_eq!(
                client
                    .register("synthetic.example.test", credentials())
                    .await,
                Err(PartnerError::Timeout)
            );
            assert_eq!(count.load(Ordering::SeqCst), stall_at + 1);
        }
    }
}
