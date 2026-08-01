use std::{
    fmt,
    future::Future,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;
use url::{Url, form_urlencoded};

use crate::tesla_stream::StreamRegion;

pub const REFRESH_RETRY_DELAY: Duration = Duration::from_secs(5 * 60);
const AUTH_FUSE_FAILURES: usize = 5;
const AUTH_FUSE_WINDOW: Duration = Duration::from_secs(10 * 60);
const TESLAMATE_USER_AGENT: &str = concat!("TeslaMate/", env!("CARGO_PKG_VERSION"));
const TESLAMATE_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8";
const TESLAMATE_ACCEPT_LANGUAGE: &str = "en-US,de-DE;q=0.5";

#[derive(Debug, Default)]
pub(crate) struct LegacyAuthFuse {
    unauthorized_at: Vec<SystemTime>,
    blown: bool,
}

impl LegacyAuthFuse {
    pub(crate) fn record_unauthorized(&mut self, now: SystemTime) {
        if self.blown {
            return;
        }
        self.unauthorized_at.retain(|at| {
            now.duration_since(*at)
                .map(|age| age < AUTH_FUSE_WINDOW)
                .unwrap_or(true)
        });
        self.unauthorized_at.push(now);
        if self.unauthorized_at.len() >= AUTH_FUSE_FAILURES {
            self.blown = true;
        }
    }

    pub(crate) fn is_blown(&self) -> bool {
        self.blown
    }

    pub(crate) fn reset(&mut self) {
        self.unauthorized_at.clear();
        self.blown = false;
    }
}
const REFRESH_SCOPE: &str = "openid email offline_access";
const CLIENT_ID: &str = "ownerapi";
const MAX_TOKEN_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LegacyAuthError {
    #[error("legacy auth request audit is unavailable")]
    AuditUnavailable,
    #[error("legacy access token is invalid")]
    InvalidAccessToken,
    #[error("legacy access token issuer is invalid")]
    InvalidIssuer,
    #[error("legacy refresh token is invalid")]
    InvalidRefreshToken,
    #[error("legacy auth response is invalid")]
    InvalidResponse,
    #[error("legacy auth response is too large")]
    ResponseTooLarge,
    #[error("legacy auth request failed")]
    Transport,
    #[error("legacy auth returned HTTP {0}")]
    HttpStatus(u16),
    #[error("legacy auth refresh is deferred")]
    RefreshDeferred,
    #[error("legacy auth clock is invalid")]
    InvalidClock,
    #[error("legacy auth persisted schedule is invalid")]
    InvalidPersistedSchedule,
    #[error("legacy credential persistence failed")]
    Persistence,
}

/// Typed, redacted terminal states for the one legacy token endpoint. Neither
/// this type nor its sink accepts a URL, header, request body, response body,
/// token, or arbitrary error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LegacyRefreshAuditOutcome {
    Success,
    HttpError(u16),
    AuthenticationRejected,
    TransportError,
    ResponseTooLarge,
    ProtocolError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LegacyRefreshAuditReceipt(pub i64);

pub(crate) trait LegacyRefreshAuditSink: Send + Sync {
    fn begin_token_refresh(&self) -> Result<LegacyRefreshAuditReceipt, LegacyAuthError>;
    fn complete_token_refresh(
        &self,
        receipt: LegacyRefreshAuditReceipt,
        outcome: LegacyRefreshAuditOutcome,
    ) -> Result<(), LegacyAuthError>;
}

#[derive(Clone)]
pub(crate) struct LegacyRefreshAuditContext {
    sink: Arc<dyn LegacyRefreshAuditSink>,
}

impl LegacyRefreshAuditContext {
    pub(crate) fn new(sink: Arc<dyn LegacyRefreshAuditSink>) -> Self {
        Self { sink }
    }

    fn begin(&self) -> Result<LegacyRefreshAuditReceipt, LegacyAuthError> {
        self.sink.begin_token_refresh()
    }

    fn complete(
        &self,
        receipt: LegacyRefreshAuditReceipt,
        outcome: LegacyRefreshAuditOutcome,
    ) -> Result<(), LegacyAuthError> {
        self.sink.complete_token_refresh(receipt, outcome)
    }
}

tokio::task_local! {
    static ACTIVE_REFRESH_AUDIT: LegacyRefreshAuditContext;
}

/// Install the required audit capability for one legacy refresh call chain.
/// The context is task-scoped and owned, so it remains valid over HTTP awaits.
pub(crate) async fn with_legacy_refresh_audit<T>(
    audit: LegacyRefreshAuditContext,
    future: impl Future<Output = T>,
) -> T {
    ACTIVE_REFRESH_AUDIT.scope(audit, future).await
}

#[derive(Clone, PartialEq, Eq)]
pub struct LegacyAuth {
    access_token: String,
    refresh_token: String,
    token_type: String,
    expires_at: i64,
    next_refresh_at: i64,
    issuer: Url,
    region: StreamRegion,
    retry_at: Option<i64>,
    #[cfg(test)]
    validate_rotated_issuer: bool,
}

impl fmt::Debug for LegacyAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LegacyAuth")
            .field("region", &self.region)
            .field("expires_at", &self.expires_at)
            .field("next_refresh_at", &self.next_refresh_at)
            .field("retry_at", &self.retry_at)
            .finish()
    }
}

impl LegacyAuth {
    pub fn from_access_token(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
    ) -> Result<Self, LegacyAuthError> {
        let access_token = access_token.into();
        let refresh_token = refresh_token.into();
        validate_refresh_token(&refresh_token)?;
        let (issuer, region) = issuer_from_access_token(&access_token)?;
        Ok(Self {
            access_token,
            refresh_token,
            token_type: "Bearer".to_owned(),
            expires_at: 0,
            next_refresh_at: 0,
            issuer,
            region,
            retry_at: None,
            #[cfg(test)]
            validate_rotated_issuer: true,
        })
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn refresh_token(&self) -> &str {
        &self.refresh_token
    }

    pub fn token_type(&self) -> &str {
        &self.token_type
    }

    pub fn region(&self) -> StreamRegion {
        self.region
    }

    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    pub fn next_refresh_at(&self) -> i64 {
        self.next_refresh_at
    }

    pub fn retry_at(&self) -> Option<i64> {
        self.retry_at
    }

    pub fn refresh_due(&self, now: SystemTime) -> Result<bool, LegacyAuthError> {
        Ok(epoch_seconds(now)? >= self.next_refresh_at)
    }

    pub async fn refresh_if_due(
        &mut self,
        client: &Client,
        now: SystemTime,
    ) -> Result<(), LegacyAuthError> {
        self.refresh_if_due_persisted(client, now, |_, _, _, _| Ok(()))
            .await
    }

    pub async fn refresh_if_due_persisted<F>(
        &mut self,
        client: &Client,
        now: SystemTime,
        persist: F,
    ) -> Result<(), LegacyAuthError>
    where
        F: FnOnce(&str, &str, i64, i64) -> Result<(), LegacyAuthError>,
    {
        let now = epoch_seconds(now)?;
        if self.retry_at.is_some_and(|retry_at| now < retry_at) {
            return Err(LegacyAuthError::RefreshDeferred);
        }
        if now < self.next_refresh_at {
            return Ok(());
        }
        self.refresh_at(client, now, persist).await
    }

    pub async fn refresh_now(
        &mut self,
        client: &Client,
        now: SystemTime,
    ) -> Result<(), LegacyAuthError> {
        self.refresh_now_persisted(client, now, |_, _, _, _| Ok(())).await
    }

    pub async fn refresh_now_persisted<F>(
        &mut self,
        client: &Client,
        now: SystemTime,
        persist: F,
    ) -> Result<(), LegacyAuthError>
    where
        F: FnOnce(&str, &str, i64, i64) -> Result<(), LegacyAuthError>,
    {
        let now = epoch_seconds(now)?;
        if self.retry_at.is_some_and(|retry_at| now < retry_at) {
            return Err(LegacyAuthError::RefreshDeferred);
        }
        self.refresh_at(client, now, persist).await
    }

    async fn refresh_at<F>(
        &mut self,
        client: &Client,
        now: i64,
        persist: F,
    ) -> Result<(), LegacyAuthError>
    where
        F: FnOnce(&str, &str, i64, i64) -> Result<(), LegacyAuthError>,
    {
        #[cfg(not(test))]
        {
            let audit = ACTIVE_REFRESH_AUDIT
                .try_with(Clone::clone)
                .map_err(|_| LegacyAuthError::AuditUnavailable)?;
            let receipt = audit.begin()?;
            let result = self.refresh_at_unchecked(client, now, persist).await;
            audit.complete(receipt, legacy_refresh_outcome(&result))?;
            return result;
        }

        #[cfg(test)]
        self.refresh_at_unchecked(client, now, persist).await
    }

    /// This is deliberately private. Non-test callers reach it only through
    /// `refresh_at`, which obtains an active audit context and persists the
    /// pre-I/O receipt before this method can call `.send()`.
    async fn refresh_at_unchecked<F>(
        &mut self,
        client: &Client,
        now: i64,
        persist: F,
    ) -> Result<(), LegacyAuthError>
    where
        F: FnOnce(&str, &str, i64, i64) -> Result<(), LegacyAuthError>,
    {
        let endpoint = self
            .issuer
            .join("token")
            .map_err(|_| LegacyAuthError::InvalidIssuer)?;
        let body = form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("scope", REFRESH_SCOPE)
            .append_pair("client_id", CLIENT_ID)
            .append_pair("refresh_token", &self.refresh_token)
            .finish();
        let response = match client
            .post(endpoint)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("user-agent", TESLAMATE_USER_AGENT)
            .header("accept", TESLAMATE_ACCEPT)
            .header("accept-language", TESLAMATE_ACCEPT_LANGUAGE)
            .body(body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return self.failed_refresh(now, LegacyAuthError::Transport),
        };
        if !response.status().is_success() {
            return self
                .failed_refresh(now, LegacyAuthError::HttpStatus(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_TOKEN_RESPONSE_BYTES as u64)
        {
            return self.failed_refresh(now, LegacyAuthError::ResponseTooLarge);
        }
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(_) => return self.failed_refresh(now, LegacyAuthError::Transport),
        };
        if bytes.len() > MAX_TOKEN_RESPONSE_BYTES {
            return self.failed_refresh(now, LegacyAuthError::ResponseTooLarge);
        }
        let response: TokenResponse = match serde_json::from_slice(&bytes) {
            Ok(response) => response,
            Err(_) => return self.failed_refresh(now, LegacyAuthError::InvalidResponse),
        };
        let rotated = match self.validate_response(response, now) {
            Ok(rotated) => rotated,
            Err(error) => return self.failed_refresh(now, error),
        };
        if persist(
            &rotated.access_token,
            &rotated.refresh_token,
            rotated.expires_at,
            rotated.next_refresh_at,
        )
        .is_err()
        {
            return self.failed_refresh(now, LegacyAuthError::Persistence);
        }
        self.access_token = rotated.access_token;
        self.refresh_token = rotated.refresh_token;
        self.token_type = rotated.token_type;
        self.expires_at = rotated.expires_at;
        self.next_refresh_at = rotated.next_refresh_at;
        self.retry_at = None;
        Ok(())
    }

    fn failed_refresh<T>(
        &mut self,
        now: i64,
        error: LegacyAuthError,
    ) -> Result<T, LegacyAuthError> {
        self.retry_at = now.checked_add(REFRESH_RETRY_DELAY.as_secs() as i64);
        Err(error)
    }

    fn validate_response(
        &self,
        response: TokenResponse,
        receipt_epoch: i64,
    ) -> Result<ValidatedTokens, LegacyAuthError> {
        validate_nonempty(&response.access_token)?;
        validate_refresh_token(&response.refresh_token)?;
        if !response.token_type.eq_ignore_ascii_case("bearer") {
            return Err(LegacyAuthError::InvalidResponse);
        }
        if response.expires_in == 0 {
            return Err(LegacyAuthError::InvalidResponse);
        }
        let created_at = response.created_at.unwrap_or(receipt_epoch);
        if response.created_at.is_some_and(|created_at| created_at <= 0)
            || created_at <= 0
        {
            return Err(LegacyAuthError::InvalidResponse);
        }
        #[cfg(test)]
        if self.validate_rotated_issuer {
            let (issuer, region) = issuer_from_access_token(&response.access_token)?;
            if issuer != self.issuer || region != self.region {
                return Err(LegacyAuthError::InvalidIssuer);
            }
        }
        #[cfg(not(test))]
        {
            let (issuer, region) = issuer_from_access_token(&response.access_token)?;
            if issuer != self.issuer || region != self.region {
                return Err(LegacyAuthError::InvalidIssuer);
            }
        }
        let expires_at = response
            .created_at
            .unwrap_or(created_at)
            .checked_add(
                i64::try_from(response.expires_in).map_err(|_| LegacyAuthError::InvalidResponse)?,
            )
            .ok_or(LegacyAuthError::InvalidResponse)?;
        let refresh_offset = response
            .expires_in
            .checked_mul(3)
            .ok_or(LegacyAuthError::InvalidResponse)?
            / 4;
        let next_refresh_at = response
            .created_at
            .unwrap_or(created_at)
            .checked_add(
                i64::try_from(refresh_offset).map_err(|_| LegacyAuthError::InvalidResponse)?,
            )
            .ok_or(LegacyAuthError::InvalidResponse)?;
        Ok(ValidatedTokens {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            token_type: "Bearer".to_owned(),
            expires_at,
            next_refresh_at,
        })
    }

    pub(crate) fn from_persisted_state(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        expires_at: i64,
        next_refresh_at: i64,
    ) -> Result<Self, LegacyAuthError> {
        let mut auth = Self::from_access_token(access_token, refresh_token)?;
        if expires_at == 0 && next_refresh_at == 0 {
            return Ok(auth);
        }
        if expires_at <= 0 || next_refresh_at <= 0 || next_refresh_at >= expires_at {
            return Err(LegacyAuthError::InvalidPersistedSchedule);
        }
        auth.expires_at = expires_at;
        auth.next_refresh_at = next_refresh_at;
        Ok(auth)
    }

    #[cfg(test)]
    pub(crate) fn for_test(issuer: Url, access_token: &str, refresh_token: &str) -> Self {
        Self {
            access_token: access_token.to_owned(),
            refresh_token: refresh_token.to_owned(),
            token_type: "Bearer".to_owned(),
            expires_at: 0,
            next_refresh_at: 0,
            region: StreamRegion::Global,
            issuer,
            retry_at: None,
            validate_rotated_issuer: false,
        }
    }
}

fn legacy_refresh_outcome(
    result: &Result<(), LegacyAuthError>,
) -> LegacyRefreshAuditOutcome {
    match result {
        Ok(()) => LegacyRefreshAuditOutcome::Success,
        Err(LegacyAuthError::Transport) => LegacyRefreshAuditOutcome::TransportError,
        Err(LegacyAuthError::ResponseTooLarge) => LegacyRefreshAuditOutcome::ResponseTooLarge,
        Err(LegacyAuthError::HttpStatus(401)) => LegacyRefreshAuditOutcome::AuthenticationRejected,
        Err(LegacyAuthError::HttpStatus(status)) => LegacyRefreshAuditOutcome::HttpError(*status),
        Err(_) => LegacyRefreshAuditOutcome::ProtocolError,
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    token_type: String,
    expires_in: u64,
    created_at: Option<i64>,
}

struct ValidatedTokens {
    access_token: String,
    refresh_token: String,
    token_type: String,
    expires_at: i64,
    next_refresh_at: i64,
}

fn issuer_from_access_token(access_token: &str) -> Result<(Url, StreamRegion), LegacyAuthError> {
    if let Some(issuer) = opaque_access_token_issuer(access_token)? {
        return validated_issuer(issuer);
    }
    let mut segments = access_token.split('.');
    let _header = segments.next().ok_or(LegacyAuthError::InvalidAccessToken)?;
    let payload = segments.next().ok_or(LegacyAuthError::InvalidAccessToken)?;
    if segments.next().is_none() || segments.next().is_some() {
        return Err(LegacyAuthError::InvalidAccessToken);
    }
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| LegacyAuthError::InvalidAccessToken)?;
    let claims: serde_json::Value =
        serde_json::from_slice(&payload).map_err(|_| LegacyAuthError::InvalidAccessToken)?;
    let issuer = claims
        .get("iss")
        .and_then(serde_json::Value::as_str)
        .ok_or(LegacyAuthError::InvalidIssuer)?;
    validated_issuer(issuer)
}

// TeslaMate accepts current opaque access tokens before attempting JWT payload
// decoding. Hub has no arbitrary issuer override, so prefix handling selects only
// Tesla's canonical regional issuer; the token itself never supplies a URL.
fn opaque_access_token_issuer(access_token: &str) -> Result<Option<&'static str>, LegacyAuthError> {
    let (prefix, issuer) = if let Some(suffix) = access_token.strip_prefix("qts-") {
        (suffix, "https://auth.tesla.com/oauth2/v3")
    } else if let Some(suffix) = access_token.strip_prefix("eu-") {
        (suffix, "https://auth.tesla.com/oauth2/v3")
    } else if let Some(suffix) = access_token.strip_prefix("cn-") {
        (suffix, "https://auth.tesla.cn/oauth2/v3")
    } else {
        return Ok(None);
    };

    if prefix.is_empty()
        || prefix
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() || byte == b' ')
    {
        return Err(LegacyAuthError::InvalidAccessToken);
    }
    Ok(Some(issuer))
}

fn validated_issuer(issuer: &str) -> Result<(Url, StreamRegion), LegacyAuthError> {
    let mut issuer = Url::parse(issuer).map_err(|_| LegacyAuthError::InvalidIssuer)?;
    if issuer.scheme() != "https"
        || issuer.username() != ""
        || issuer.password().is_some()
        || issuer.port().is_some_and(|port| port != 443)
        || issuer.query().is_some()
        || issuer.fragment().is_some()
    {
        return Err(LegacyAuthError::InvalidIssuer);
    }
    if issuer.port() == Some(443) {
        issuer
            .set_port(None)
            .map_err(|_| LegacyAuthError::InvalidIssuer)?;
    }
    let region = match issuer.host_str() {
        Some("auth.tesla.com") => StreamRegion::Global,
        Some("auth.tesla.cn") => StreamRegion::China,
        _ => return Err(LegacyAuthError::InvalidIssuer),
    };
    if issuer.path() != "/oauth2/v3" && issuer.path() != "/oauth2/v3/" {
        return Err(LegacyAuthError::InvalidIssuer);
    }
    if !issuer.path().ends_with('/') {
        issuer.set_path(&format!("{}/", issuer.path()));
    }
    Ok((issuer, region))
}

fn validate_nonempty(value: &str) -> Result<(), LegacyAuthError> {
    if value.trim().is_empty() {
        Err(LegacyAuthError::InvalidResponse)
    } else {
        Ok(())
    }
}

fn validate_refresh_token(value: &str) -> Result<(), LegacyAuthError> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        Err(LegacyAuthError::InvalidRefreshToken)
    } else {
        Ok(())
    }
}

fn epoch_seconds(now: SystemTime) -> Result<i64, LegacyAuthError> {
    now.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .ok_or(LegacyAuthError::InvalidClock)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Router,
        extract::State,
        http::{HeaderMap, StatusCode, header::{ACCEPT, ACCEPT_LANGUAGE, USER_AGENT}},
        response::IntoResponse,
        routing::{get, post},
    };
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use tokio::net::TcpListener;

    use super::*;
    use crate::owner_api::{OwnerApi, OwnerApiAuthError, OwnerApiError};

    fn access_token(issuer: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::json!({"iss": issuer}).to_string());
        format!("{header}.{payload}.signature")
    }

    #[test]
    fn derives_safe_global_and_china_issuer_regions() {
        let global = LegacyAuth::from_access_token(
            access_token("https://auth.tesla.com/oauth2/v3"),
            "refresh",
        )
        .unwrap();
        assert_eq!(global.region(), StreamRegion::Global);
        let china = LegacyAuth::from_access_token(
            access_token("https://auth.tesla.cn/oauth2/v3/"),
            "refresh",
        )
        .unwrap();
        assert_eq!(china.region(), StreamRegion::China);
        assert!(
            LegacyAuth::from_access_token(
                access_token("https://evil.example/oauth2/v3"),
                "refresh",
            )
            .is_err()
        );
        assert!(LegacyAuth::from_access_token(
            access_token("https://auth.tesla.com:8443/oauth2/v3"),
            "refresh",
        )
        .is_err());
        let default_port = LegacyAuth::from_access_token(
            access_token("https://auth.tesla.com:443/oauth2/v3"),
            "refresh",
        )
        .unwrap();
        assert_eq!(default_port.issuer, global.issuer);
    }

    #[test]
    fn derives_canonical_issuers_for_teslamate_opaque_access_tokens() {
        for token in ["qts-access-token", "eu-access-token"] {
            let auth = LegacyAuth::from_access_token(token, "refresh").unwrap();
            assert_eq!(auth.region(), StreamRegion::Global);
        }
        let china = LegacyAuth::from_access_token("cn-access-token", "refresh").unwrap();
        assert_eq!(china.region(), StreamRegion::China);

        for token in ["qts-", "eu-\nsecret", "cn- secret"] {
            assert_eq!(
                LegacyAuth::from_access_token(token, "refresh").unwrap_err(),
                LegacyAuthError::InvalidAccessToken
            );
        }
    }

    #[derive(Clone, Default)]
    struct MockState {
        bodies: Arc<Mutex<Vec<String>>>,
        request_headers: Arc<Mutex<Vec<(String, String, String)>>>,
        token_response: Arc<Mutex<(StatusCode, String)>>,
        unauthorized_count: Arc<Mutex<usize>>,
    }

    async fn token_handler(
        State(state): State<MockState>,
        headers: HeaderMap,
        body: String,
    ) -> impl IntoResponse {
        state.bodies.lock().unwrap().push(body);
        state.request_headers.lock().unwrap().push((
            headers
                .get(USER_AGENT)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
            headers
                .get(ACCEPT)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
            headers
                .get(ACCEPT_LANGUAGE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
        ));
        state.token_response.lock().unwrap().clone()
    }

    async fn products_handler(State(state): State<MockState>) -> impl IntoResponse {
        let mut count = state.unauthorized_count.lock().unwrap();
        if *count > 0 {
            *count -= 1;
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
        (StatusCode::OK, r#"{"response":[],"count":0}"#).into_response()
    }

    async fn mock_server(state: MockState) -> (Url, tokio::task::JoinHandle<()>) {
        crate::crypto::install_default_provider();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/oauth2/v3/token", post(token_handler))
                    .route("/api/1/products", get(products_handler))
                    .with_state(state),
            )
            .await
            .unwrap();
        });
        (
            Url::parse(&format!("http://{address}/oauth2/v3/")).unwrap(),
            task,
        )
    }

    fn valid_response(access_token: &str, refresh_token: &str) -> String {
        serde_json::json!({
            "access_token": access_token,
            "refresh_token": refresh_token,
            "token_type": "Bearer",
            "expires_in": 1000,
            "created_at": 1_700_000_000,
        })
        .to_string()
    }

    #[tokio::test]
    async fn posts_exact_refresh_form_and_schedules_at_seventy_five_percent() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                valid_response("new-access", "new-refresh"),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let mut auth =
            crate::credentials::LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
        auth.refresh_now(
            &Client::new(),
            UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        )
        .await
        .unwrap();
        assert_eq!(auth.access_token(), "new-access");
        assert_eq!(auth.refresh_token(), "new-refresh");
        assert_eq!(auth.next_refresh_at(), 1_700_000_750);
        assert_eq!(
            state.bodies.lock().unwrap().as_slice(),
            &[
                "grant_type=refresh_token&scope=openid+email+offline_access&client_id=ownerapi&refresh_token=old-refresh"
            ]
        );
        assert_eq!(
            state.request_headers.lock().unwrap().as_slice(),
            &[ (
                TESLAMATE_USER_AGENT.to_owned(),
                TESLAMATE_ACCEPT.to_owned(),
                TESLAMATE_ACCEPT_LANGUAGE.to_owned(),
            ) ]
        );
    }

    #[tokio::test]
    async fn missing_created_at_uses_validated_local_refresh_epoch() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                serde_json::json!({
                    "access_token": "new-access",
                    "refresh_token": "new-refresh",
                    "token_type": "bEaReR",
                    "expires_in": 1000,
                })
                .to_string(),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state).await;
        let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        auth.refresh_now(
            &Client::new(),
            UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        )
        .await
        .unwrap();
        assert_eq!(auth.expires_at(), 1_700_001_000);
        assert_eq!(auth.next_refresh_at(), 1_700_000_750);
    }

    #[tokio::test]
    async fn invalid_or_missing_token_type_is_rejected() {
        for response in [
            serde_json::json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "token_type": "MAC",
                "expires_in": 1000,
                "created_at": 1_700_000_000,
            })
            .to_string(),
            serde_json::json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "expires_in": 1000,
                "created_at": 1_700_000_000,
            })
            .to_string(),
        ] {
            let state = MockState {
                token_response: Arc::new(Mutex::new((StatusCode::OK, response))),
                ..MockState::default()
            };
            let (issuer, _task) = mock_server(state).await;
            let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
            assert_eq!(
                auth.refresh_now(
                    &Client::new(),
                    UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                )
                .await
                .unwrap_err(),
                LegacyAuthError::InvalidResponse
            );
            assert_eq!(auth.access_token(), "old-access");
            assert_eq!(auth.refresh_token(), "old-refresh");
        }
    }

    #[tokio::test]
    async fn html_or_non_json_response_is_rejected_without_rotation() {
        for body in ["<html>not a token</html>", "not-json"] {
            let state = MockState {
                token_response: Arc::new(Mutex::new((StatusCode::OK, body.to_owned()))),
                ..MockState::default()
            };
            let (issuer, _task) = mock_server(state).await;
            let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
            assert_eq!(
                auth.refresh_now(
                    &Client::new(),
                    UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                )
                .await
                .unwrap_err(),
                LegacyAuthError::InvalidResponse
            );
            assert_eq!(auth.access_token(), "old-access");
            assert_eq!(auth.refresh_token(), "old-refresh");
        }
    }

    #[tokio::test]
    async fn invalid_response_rolls_back_and_sets_five_minute_retry_without_redaction_leak() {
        let secret = "old-refresh-secret";
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                r#"{"access_token":"new-access","token_type":"Bearer","expires_in":1000,"created_at":1700000000}"#.to_owned(),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state).await;
        let mut auth = LegacyAuth::for_test(issuer, "old-access", secret);
        let error = auth
            .refresh_now(
                &Client::new(),
                UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            )
            .await
            .unwrap_err();
        assert_eq!(error, LegacyAuthError::InvalidResponse);
        assert_eq!(auth.refresh_token(), secret);
        assert_eq!(auth.retry_at(), Some(1_700_000_300));
        assert!(!format!("{auth:?}").contains(secret));
        assert!(!error.to_string().contains(secret));
    }

    #[tokio::test]
    async fn owner_api_refreshes_once_on_401_then_retries_once_without_loop() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                valid_response("new-access", "new-refresh"),
            ))),
            unauthorized_count: Arc::new(Mutex::new(1)),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let client =
            OwnerApi::for_fake_http(issuer.join("../../").unwrap(), Duration::from_secs(2))
                .unwrap();
        let auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let mut auth =
            crate::credentials::LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
        let vehicles = client
            .list_vehicles_with_legacy_auth(&mut auth)
            .await
            .unwrap();
        assert!(vehicles.is_empty());
        assert_eq!(state.bodies.lock().unwrap().len(), 1);

        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                valid_response("new-access", "new-refresh"),
            ))),
            unauthorized_count: Arc::new(Mutex::new(2)),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let client =
            OwnerApi::for_fake_http(issuer.join("../../").unwrap(), Duration::from_secs(2))
                .unwrap();
        let auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let mut auth =
            crate::credentials::LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
        let error = client
            .list_vehicles_with_legacy_auth(&mut auth)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401))
        ));
        assert_eq!(*state.unauthorized_count.lock().unwrap(), 0);
        assert_eq!(state.bodies.lock().unwrap().len(), 1);
    }

    #[test]
    fn account_unauthorized_fuse_is_shared_windowed_and_resettable() {
        let base = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut fuse = LegacyAuthFuse::default();
        for offset in 0..4 {
            fuse.record_unauthorized(base + Duration::from_secs(offset));
            assert!(!fuse.is_blown());
        }
        fuse.record_unauthorized(base + Duration::from_secs(9 * 60));
        assert!(fuse.is_blown());

        fuse.reset();
        assert!(!fuse.is_blown());
        fuse.record_unauthorized(base + Duration::from_secs(20 * 60));
        assert!(!fuse.is_blown());
        fuse.record_unauthorized(base + Duration::from_secs(20 * 60 + 601));
        assert!(!fuse.is_blown());
    }
}
