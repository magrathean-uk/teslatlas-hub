use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(all(test, unix))]
use std::sync::atomic::{AtomicBool, Ordering};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::tesla_stream::StreamRegion;

pub const REFRESH_RETRY_DELAY: Duration = Duration::from_secs(5 * 60);
pub const STARTUP_REFRESH_RETRY_DELAY: Duration = Duration::from_secs(450);
const AUTH_FUSE_FAILURES: usize = 5;
const AUTH_FUSE_WINDOW: Duration = Duration::from_secs(10 * 60);
const TESLAMATE_USER_AGENT: &str = "TeslaMate/4.1.0-dev";
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
        // Pinned TeslaMate tolerates five melts and blows on the sixth.
        if self.unauthorized_at.len() > AUTH_FUSE_FAILURES {
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

// This exists only in libtest builds. The parent characterisation test starts
// a clean-environment child with this exact value, then verifies that SIGKILL
// leaves the pre-response durable marker fenced on restart.
#[cfg(all(test, unix))]
pub(crate) const TEST_KILL_AFTER_VALIDATED_ROTATION_ENV: &str =
    "TESLATLAS_HUB_TEST_KILL_AFTER_VALIDATED_ROTATION";
#[cfg(all(test, unix))]
pub(crate) const TEST_KILL_AFTER_VALIDATED_ROTATION_VALUE: &str =
    "phase2-response-before-first-persist-v1";
#[cfg(all(test, unix))]
static TEST_KILL_AFTER_VALIDATED_ROTATION_USED: AtomicBool = AtomicBool::new(false);

#[cfg(all(test, unix))]
fn test_kill_after_validated_rotation_if_armed() {
    if std::env::var(TEST_KILL_AFTER_VALIDATED_ROTATION_ENV)
        .ok()
        .as_deref()
        != Some(TEST_KILL_AFTER_VALIDATED_ROTATION_VALUE)
    {
        return;
    }
    if TEST_KILL_AFTER_VALIDATED_ROTATION_USED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    // SIGKILL gives the child no cleanup/destructor path. Use an absolute
    // system utility so this test-only path remains compatible with the
    // crate-wide `forbid(unsafe_code)` policy.
    let status = std::process::Command::new("/bin/kill")
        .arg("-KILL")
        .arg(std::process::id().to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("test SIGKILL helper must start");
    debug_assert!(status.success());
    std::process::abort();
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LegacyAuthError {
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
    #[error("legacy refresh rotation outcome is unknown")]
    RotationOutcomeUnknown,
    #[error("legacy refresh rotation outcome is unknown while persistence authority is active")]
    SensitiveRotationOutcomeUnknown,
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
    #[error("legacy credential persistence authority is unavailable")]
    SensitivePersistenceUnavailable,
    #[error("legacy credential authority is unavailable")]
    SensitiveAccessUnavailable,
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
    /// A newly imported pair has no trusted schedule and must refresh once.
    /// A pair persisted by Hub keeps its schedule across process restarts.
    startup_refresh_pending: bool,
    /// A token response was accepted, but the rotated pair has not yet been
    /// durably committed. While this is set, refresh calls retry persistence
    /// only; they must never submit the now-consumed previous refresh token.
    persistence_pending: bool,
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
            startup_refresh_pending: false,
            persistence_pending: false,
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
        let now = epoch_seconds(now)?;
        Ok(self.persistence_pending || self.startup_refresh_pending || now >= self.next_refresh_at)
    }

    pub async fn refresh_if_due(
        &mut self,
        client: &Client,
        now: SystemTime,
    ) -> Result<(), LegacyAuthError> {
        if self.persistence_pending {
            return Err(LegacyAuthError::Persistence);
        }
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
        if self.persistence_pending {
            return self.retry_pending_persistence(now, persist);
        }
        if !self.startup_refresh_pending && self.retry_at.is_none() && now < self.next_refresh_at {
            return Ok(());
        }
        self.refresh_at(client, now, persist).await
    }

    pub async fn refresh_now(
        &mut self,
        client: &Client,
        now: SystemTime,
    ) -> Result<(), LegacyAuthError> {
        if self.persistence_pending {
            return Err(LegacyAuthError::Persistence);
        }
        self.refresh_now_persisted(client, now, |_, _, _, _| Ok(()))
            .await
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
        if self.persistence_pending {
            return self.retry_pending_persistence(now, persist);
        }
        self.refresh_at(client, now, persist).await
    }

    /// Run the refresh state machine with an audit receipt already bound by the
    /// credential journal manager. The caller must have persisted both the
    /// journal's `RefreshAttempted` state and its durable receipt binding before
    /// this method can reach the HTTP send.
    #[cfg(test)]
    pub(crate) async fn refresh_persisted_with_bound_audit<F>(
        &mut self,
        client: &Client,
        now: SystemTime,
        force: bool,
        persist: F,
    ) -> Result<(), LegacyAuthError>
    where
        F: FnOnce(&str, &str, i64, i64) -> Result<(), LegacyAuthError>,
    {
        self.refresh_persisted_with_bound_audit_guarded(client, now, force, || Ok(()), persist)
            .await
    }

    /// Same bound-audit refresh path, with a caller-owned authority check at
    /// the deepest possible boundary: after request construction and directly
    /// before the token endpoint send.
    pub(crate) async fn refresh_persisted_with_bound_audit_guarded<F, G>(
        &mut self,
        client: &Client,
        now: SystemTime,
        force: bool,
        assert_sensitive_access: G,
        persist: F,
    ) -> Result<(), LegacyAuthError>
    where
        F: FnOnce(&str, &str, i64, i64) -> Result<(), LegacyAuthError>,
        G: FnOnce() -> Result<(), LegacyAuthError>,
    {
        let now = epoch_seconds(now)?;
        if !force && self.retry_at.is_some_and(|retry_at| now < retry_at) {
            return Err(LegacyAuthError::RefreshDeferred);
        }
        if self.persistence_pending {
            return self.retry_pending_persistence(now, persist);
        }
        if !force
            && !self.startup_refresh_pending
            && self.retry_at.is_none()
            && now < self.next_refresh_at
        {
            return Ok(());
        }
        self.refresh_at_unchecked(client, now, assert_sensitive_access, persist)
            .await
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
        self.refresh_at_unchecked(client, now, || Ok(()), persist)
            .await
    }

    /// This is the final guarded boundary before the single-use refresh POST.
    async fn refresh_at_unchecked<F, G>(
        &mut self,
        client: &Client,
        receipt_epoch: i64,
        assert_sensitive_access: G,
        persist: F,
    ) -> Result<(), LegacyAuthError>
    where
        F: FnOnce(&str, &str, i64, i64) -> Result<(), LegacyAuthError>,
        G: FnOnce() -> Result<(), LegacyAuthError>,
    {
        let endpoint = match self.issuer.join("token") {
            Ok(endpoint) => endpoint,
            Err(_) => {
                return self.failed_refresh(receipt_epoch, LegacyAuthError::InvalidIssuer);
            }
        };
        let body = TokenRequest {
            grant_type: "refresh_token",
            scope: REFRESH_SCOPE,
            client_id: CLIENT_ID,
            refresh_token: &self.refresh_token,
        };
        let body = match serde_json::to_vec(&body) {
            Ok(body) => body,
            Err(_) => {
                return self.failed_refresh(receipt_epoch, LegacyAuthError::InvalidResponse);
            }
        };
        let request = client
            .post(endpoint)
            .header("content-type", "application/json")
            .header("user-agent", TESLAMATE_USER_AGENT)
            .header("accept", TESLAMATE_ACCEPT)
            .header("accept-language", TESLAMATE_ACCEPT_LANGUAGE)
            .body(body);
        assert_sensitive_access()?;
        let response = match request.send().await {
            Ok(response) => response,
            Err(_) => return self.failed_refresh(receipt_epoch, LegacyAuthError::Transport),
        };
        if response.status() != reqwest::StatusCode::OK {
            return self.failed_refresh(
                receipt_epoch,
                LegacyAuthError::HttpStatus(response.status().as_u16()),
            );
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_TOKEN_RESPONSE_BYTES as u64)
        {
            return self.failed_refresh(receipt_epoch, LegacyAuthError::ResponseTooLarge);
        }
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(_) => return self.failed_refresh(receipt_epoch, LegacyAuthError::Transport),
        };
        if bytes.len() > MAX_TOKEN_RESPONSE_BYTES {
            return self.failed_refresh(receipt_epoch, LegacyAuthError::ResponseTooLarge);
        }
        let response: TokenResponse = match serde_json::from_slice(&bytes) {
            Ok(response) => response,
            Err(_) => return self.failed_refresh(receipt_epoch, LegacyAuthError::InvalidResponse),
        };
        let rotated = match self.validate_response(response, receipt_epoch) {
            Ok(rotated) => rotated,
            Err(error) => return self.failed_refresh(receipt_epoch, error),
        };
        #[cfg(all(test, unix))]
        test_kill_after_validated_rotation_if_armed();
        // Install the validated rotation in memory before attempting durable
        // persistence. Tesla refresh tokens are single-use: falling back to
        // the previous pair after this point can strand authentication. A
        // failed sink therefore leaves the new pair pending and every later
        // refresh call retries only that persistence operation.
        self.access_token = rotated.access_token;
        self.refresh_token = rotated.refresh_token;
        self.token_type = rotated.token_type;
        self.expires_at = rotated.expires_at;
        self.next_refresh_at = rotated.next_refresh_at;
        self.persistence_pending = true;
        self.startup_refresh_pending = false;
        if let Err(error) = persist(
            &self.access_token,
            &self.refresh_token,
            self.expires_at,
            self.next_refresh_at,
        ) {
            return self.failed_refresh(receipt_epoch, error);
        }
        self.persistence_pending = false;
        self.retry_at = None;
        Ok(())
    }

    fn retry_pending_persistence<F>(&mut self, now: i64, persist: F) -> Result<(), LegacyAuthError>
    where
        F: FnOnce(&str, &str, i64, i64) -> Result<(), LegacyAuthError>,
    {
        debug_assert!(self.persistence_pending);
        if let Err(error) = persist(
            &self.access_token,
            &self.refresh_token,
            self.expires_at,
            self.next_refresh_at,
        ) {
            return self.failed_refresh(now, error);
        }
        self.persistence_pending = false;
        self.retry_at = None;
        Ok(())
    }

    fn failed_refresh<T>(
        &mut self,
        now: i64,
        error: LegacyAuthError,
    ) -> Result<T, LegacyAuthError> {
        let delay = if self.startup_refresh_pending {
            STARTUP_REFRESH_RETRY_DELAY
        } else {
            REFRESH_RETRY_DELAY
        };
        self.startup_refresh_pending = false;
        self.retry_at = now.checked_add(delay.as_secs() as i64);
        Err(error)
    }

    fn validate_response(
        &self,
        response: TokenResponse,
        receipt_epoch: i64,
    ) -> Result<ValidatedTokens, LegacyAuthError> {
        validate_nonempty(&response.access_token)?;
        validate_refresh_token(&response.refresh_token)
            .map_err(|_| LegacyAuthError::InvalidResponse)?;
        if response.expires_in == 0 {
            return Err(LegacyAuthError::InvalidResponse);
        }
        if receipt_epoch <= 0 {
            return Err(LegacyAuthError::InvalidResponse);
        }
        let expires_at = receipt_epoch
            .checked_add(
                i64::try_from(response.expires_in).map_err(|_| LegacyAuthError::InvalidResponse)?,
            )
            .ok_or(LegacyAuthError::InvalidResponse)?;
        let refresh_offset = teslamate_refresh_delay_seconds(response.expires_in)?;
        let next_refresh_at = receipt_epoch
            .checked_add(
                i64::try_from(refresh_offset).map_err(|_| LegacyAuthError::InvalidResponse)?,
            )
            .ok_or(LegacyAuthError::InvalidResponse)?;
        Ok(ValidatedTokens {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            token_type: response.token_type.unwrap_or_else(|| "Bearer".to_owned()),
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
            auth.startup_refresh_pending = true;
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
            startup_refresh_pending: false,
            persistence_pending: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_schedule(mut self, expires_at: i64, next_refresh_at: i64) -> Self {
        assert!(next_refresh_at > 0 && next_refresh_at < expires_at);
        self.expires_at = expires_at;
        self.next_refresh_at = next_refresh_at;
        self
    }
}

#[derive(Debug, Serialize)]
struct TokenRequest<'a> {
    grant_type: &'static str,
    scope: &'static str,
    client_id: &'static str,
    refresh_token: &'a str,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    token_type: Option<String>,
    expires_in: u64,
    #[serde(rename = "created_at")]
    _created_at: Option<i64>,
}

struct ValidatedTokens {
    access_token: String,
    refresh_token: String,
    token_type: String,
    expires_at: i64,
    next_refresh_at: i64,
}

fn issuer_from_access_token(access_token: &str) -> Result<(Url, StreamRegion), LegacyAuthError> {
    if access_token.trim().is_empty() || access_token.chars().any(char::is_control) {
        return Err(LegacyAuthError::InvalidAccessToken);
    }
    if let Some(issuer) = opaque_access_token_issuer(access_token)? {
        return validated_issuer(issuer);
    }
    let mut segments = access_token.split('.');
    let (_header, payload, signature, extra) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    );
    if let (Some(_), Some(payload), Some(_), None) = (_header, payload, signature, extra)
        && let Ok(payload) = URL_SAFE_NO_PAD.decode(payload)
        && let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&payload)
        && let Some(issuer) = claims.get("iss").and_then(serde_json::Value::as_str)
    {
        return validated_issuer(issuer);
    }

    // Pinned TeslaMate falls back to the canonical global Owner API issuer
    // when an access token is neither a recognised opaque token nor a usable
    // JWT. Legacy tokens are intentionally not rejected merely for that.
    validated_issuer("https://auth.tesla.com/oauth2/v3")
}

// TeslaMate accepts current opaque access tokens before attempting JWT payload
// decoding. Hub has no arbitrary issuer override, so prefix handling selects only
// Tesla's canonical regional issuer; the token itself never supplies a URL.
fn opaque_access_token_issuer(access_token: &str) -> Result<Option<&'static str>, LegacyAuthError> {
    // `cn-` also uses the global issuer: opaque tokens do not authorize
    // regional routing.
    if !(access_token.starts_with("qts-")
        || access_token.starts_with("eu-")
        || access_token.starts_with("cn-"))
    {
        return Ok(None);
    }
    Ok(Some("https://auth.tesla.com/oauth2/v3"))
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

fn teslamate_refresh_delay_seconds(expires_in: u64) -> Result<u64, LegacyAuthError> {
    expires_in
        .checked_mul(3)
        .and_then(|seconds| seconds.checked_add(2))
        .map(|seconds| seconds / 4)
        .ok_or(LegacyAuthError::InvalidResponse)
}

fn epoch_seconds(now: SystemTime) -> Result<i64, LegacyAuthError> {
    now.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .ok_or(LegacyAuthError::InvalidClock)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Router,
        extract::State,
        http::{
            HeaderMap, StatusCode,
            header::{ACCEPT, ACCEPT_LANGUAGE, USER_AGENT},
        },
        response::IntoResponse,
        routing::{get, post},
    };
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use tokio::{net::TcpListener, sync::Notify};

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
        assert!(
            LegacyAuth::from_access_token(
                access_token("https://auth.tesla.com:8443/oauth2/v3"),
                "refresh",
            )
            .is_err()
        );
        let default_port = LegacyAuth::from_access_token(
            access_token("https://auth.tesla.com:443/oauth2/v3"),
            "refresh",
        )
        .unwrap();
        assert_eq!(default_port.issuer, global.issuer);
    }

    #[test]
    fn derives_canonical_issuers_for_teslamate_opaque_access_tokens() {
        for token in ["qts-access-token", "eu-access-token", "qts-"] {
            let auth = LegacyAuth::from_access_token(token, "refresh").unwrap();
            assert_eq!(auth.region(), StreamRegion::Global);
        }
        let china = LegacyAuth::from_access_token("cn-access-token", "refresh").unwrap();
        assert_eq!(china.region(), StreamRegion::Global);

        for token in ["eu-\nsecret", "cn-\0secret"] {
            assert_eq!(
                LegacyAuth::from_access_token(token, "refresh").unwrap_err(),
                LegacyAuthError::InvalidAccessToken
            );
        }

        let fallback = LegacyAuth::from_access_token("legacy-opaque-token", "refresh").unwrap();
        assert_eq!(fallback.region(), StreamRegion::Global);
        assert_eq!(
            fallback.issuer.as_str(),
            "https://auth.tesla.com/oauth2/v3/"
        );
    }

    #[derive(Clone, Default)]
    struct MockState {
        bodies: Arc<Mutex<Vec<String>>>,
        request_headers: Arc<Mutex<Vec<(String, String, String)>>>,
        token_response: Arc<Mutex<(StatusCode, String)>>,
        unauthorized_count: Arc<Mutex<usize>>,
        token_request_count: Arc<AtomicUsize>,
        first_token_request_started: Arc<Notify>,
        block_first_token_request: Option<Arc<Notify>>,
        token_redirects_remaining: Arc<Mutex<usize>>,
        token_redirect_location: Arc<Mutex<String>>,
        redirect_capture_requests: Arc<AtomicUsize>,
        redirect_capture_body_bytes: Arc<AtomicUsize>,
        redirect_capture_authorization: Arc<AtomicUsize>,
    }

    async fn token_handler(
        State(state): State<MockState>,
        headers: HeaderMap,
        body: String,
    ) -> impl IntoResponse {
        let attempt = state.token_request_count.fetch_add(1, Ordering::SeqCst);
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
        if attempt == 0
            && let Some(blocker) = &state.block_first_token_request
        {
            state.first_token_request_started.notify_one();
            blocker.notified().await;
        }
        let redirect = {
            let mut remaining = state.token_redirects_remaining.lock().unwrap();
            if *remaining == 0 {
                None
            } else {
                *remaining -= 1;
                Some(state.token_redirect_location.lock().unwrap().clone())
            }
        };
        if let Some(location) = redirect {
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [(axum::http::header::LOCATION, location)],
                "redirect",
            )
                .into_response();
        }
        state.token_response.lock().unwrap().clone().into_response()
    }

    async fn redirect_capture_handler(
        State(state): State<MockState>,
        headers: HeaderMap,
        body: String,
    ) -> impl IntoResponse {
        state
            .redirect_capture_requests
            .fetch_add(1, Ordering::SeqCst);
        if !body.is_empty() {
            state
                .redirect_capture_body_bytes
                .fetch_add(body.len(), Ordering::SeqCst);
        }
        if headers.get("authorization").is_some() {
            state
                .redirect_capture_authorization
                .fetch_add(1, Ordering::SeqCst);
        }
        (StatusCode::INTERNAL_SERVER_ERROR, "redirect capture")
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
                    .route(
                        "/oauth2/v3/redirect-capture",
                        post(redirect_capture_handler),
                    )
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
    async fn posts_exact_teslamate_json_and_schedules_at_seventy_five_percent() {
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
        let bodies = state.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&bodies[0]).unwrap(),
            serde_json::json!({
                "grant_type": "refresh_token",
                "scope": "openid email offline_access",
                "client_id": "ownerapi",
                "refresh_token": "old-refresh"
            })
        );
        assert_eq!(
            state.request_headers.lock().unwrap().as_slice(),
            &[(
                TESLAMATE_USER_AGENT.to_owned(),
                TESLAMATE_ACCEPT.to_owned(),
                TESLAMATE_ACCEPT_LANGUAGE.to_owned(),
            )]
        );
    }

    #[tokio::test]
    async fn persistence_failure_keeps_rotated_pair_and_retries_without_second_refresh() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                valid_response("new-access", "new-refresh"),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let mut auth = LegacyAuth::for_test(issuer.clone(), "old-access", "old-refresh");
        let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        let error = auth
            .refresh_now_persisted(&Client::new(), refresh_epoch, |_, _, _, _| {
                Err(LegacyAuthError::Persistence)
            })
            .await
            .unwrap_err();
        assert_eq!(error, LegacyAuthError::Persistence);
        assert_eq!(auth.access_token(), "new-access");
        assert_eq!(auth.refresh_token(), "new-refresh");
        assert_eq!(auth.retry_at(), Some(1_700_000_300));
        assert_eq!(state.bodies.lock().unwrap().len(), 1);

        // The convenience API has no durable sink and must not be allowed to
        // erase the pending-persistence state.
        assert_eq!(
            auth.refresh_now(&Client::new(), refresh_epoch + REFRESH_RETRY_DELAY,)
                .await
                .unwrap_err(),
            LegacyAuthError::Persistence
        );

        let persisted = Arc::new(Mutex::new(Vec::new()));
        let persisted_for_sink = Arc::clone(&persisted);
        auth.refresh_now_persisted(
            &Client::new(),
            refresh_epoch + REFRESH_RETRY_DELAY,
            move |access, refresh, expires_at, next_refresh_at| {
                persisted_for_sink.lock().unwrap().push((
                    access.to_owned(),
                    refresh.to_owned(),
                    expires_at,
                    next_refresh_at,
                ));
                Ok(())
            },
        )
        .await
        .unwrap();

        assert_eq!(state.bodies.lock().unwrap().len(), 1);
        assert_eq!(auth.retry_at(), None);
        assert!(
            !auth
                .refresh_due(refresh_epoch + REFRESH_RETRY_DELAY)
                .unwrap()
        );
        assert_eq!(
            persisted.lock().unwrap().as_slice(),
            &[(
                "new-access".to_owned(),
                "new-refresh".to_owned(),
                1_700_001_000,
                1_700_000_750,
            )]
        );
    }

    #[tokio::test]
    async fn persistence_sink_error_is_preserved_on_initial_attempt_and_retry() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                valid_response("new-access", "new-refresh"),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        for now in [refresh_epoch, refresh_epoch + REFRESH_RETRY_DELAY] {
            assert_eq!(
                auth.refresh_now_persisted(&Client::new(), now, |_, _, _, _| {
                    Err(LegacyAuthError::SensitivePersistenceUnavailable)
                })
                .await
                .expect_err("sink error must remain typed"),
                LegacyAuthError::SensitivePersistenceUnavailable
            );
        }
        assert_eq!(state.bodies.lock().unwrap().len(), 1);
        assert_eq!(auth.access_token(), "new-access");
        assert_eq!(auth.refresh_token(), "new-refresh");
    }

    #[tokio::test]
    async fn credential_manager_retries_failed_sink_with_rotated_pair_only() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                valid_response("new-access", "new-refresh"),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let attempts_for_sink = Arc::clone(&attempts);
        let auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let mut manager = crate::credentials::LegacyAuthManager::for_test(
            auth,
            Arc::new(move |access, refresh| {
                let mut attempts = attempts_for_sink.lock().unwrap();
                attempts.push((access.to_owned(), refresh.to_owned()));
                if attempts.len() == 1 {
                    Err(crate::credentials::CredentialError::LegacyTokenStateWrite)
                } else {
                    Ok(())
                }
            }),
        );
        let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        assert!(matches!(
            manager.refresh_now(&Client::new(), refresh_epoch).await,
            Err(crate::credentials::LegacyAuthManagerError::Auth(
                LegacyAuthError::Persistence
            ))
        ));
        manager
            .refresh_if_due(&Client::new(), refresh_epoch + REFRESH_RETRY_DELAY)
            .await
            .unwrap();

        assert_eq!(state.bodies.lock().unwrap().len(), 1);
        assert_eq!(manager.access_token(), "new-access");
        assert_eq!(manager.refresh_token(), "new-refresh");
        assert_eq!(
            attempts.lock().unwrap().as_slice(),
            &[
                ("new-access".to_owned(), "new-refresh".to_owned()),
                ("new-access".to_owned(), "new-refresh".to_owned()),
            ]
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
    async fn provider_created_at_does_not_move_receipt_based_schedule() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                serde_json::json!({
                    "access_token": "new-access",
                    "refresh_token": "new-refresh",
                    "token_type": "Bearer",
                    "expires_in": 1000,
                    "created_at": 1,
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

    #[test]
    fn refresh_delay_matches_teslamate_rounding_at_quarter_boundaries() {
        assert_eq!(teslamate_refresh_delay_seconds(1).unwrap(), 1);
        assert_eq!(teslamate_refresh_delay_seconds(2).unwrap(), 2);
        assert_eq!(teslamate_refresh_delay_seconds(3).unwrap(), 2);
        assert_eq!(teslamate_refresh_delay_seconds(4).unwrap(), 3);
    }

    #[tokio::test]
    async fn persisted_startup_refresh_failure_uses_450_seconds_then_300() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::INTERNAL_SERVER_ERROR,
                "retry later".to_owned(),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        auth.expires_at = 1_800_000_000;
        auth.next_refresh_at = 1_750_000_000;
        auth.startup_refresh_pending = true;
        let startup = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        assert_eq!(
            auth.refresh_if_due(&Client::new(), startup)
                .await
                .unwrap_err(),
            LegacyAuthError::HttpStatus(500)
        );
        assert_eq!(auth.retry_at(), Some(1_700_000_450));
        assert_eq!(auth.access_token(), "old-access");
        assert_eq!(auth.refresh_token(), "old-refresh");

        assert_eq!(
            auth.refresh_if_due(&Client::new(), startup + STARTUP_REFRESH_RETRY_DELAY,)
                .await
                .unwrap_err(),
            LegacyAuthError::HttpStatus(500)
        );
        assert_eq!(auth.retry_at(), Some(1_700_000_750));
        assert_eq!(state.bodies.lock().unwrap().len(), 2);
    }

    #[test]
    fn persisted_reload_honours_wire_schedule() {
        assert_eq!(
            LegacyAuth::from_persisted_state("old-access", "old-refresh", 100, 200).unwrap_err(),
            LegacyAuthError::InvalidPersistedSchedule
        );
        let auth = LegacyAuth::from_persisted_state(
            "old-access",
            "old-refresh",
            1_800_000_000,
            1_750_000_000,
        )
        .unwrap();
        assert_eq!(auth.expires_at(), 1_800_000_000);
        assert_eq!(auth.next_refresh_at(), 1_750_000_000);
        assert!(
            !auth
                .refresh_due(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
                .unwrap()
        );
        assert!(
            auth.refresh_due(UNIX_EPOCH + Duration::from_secs(1_750_000_000))
                .unwrap()
        );

        let imported = LegacyAuth::from_persisted_state("old-access", "old-refresh", 0, 0).unwrap();
        assert!(
            imported
                .refresh_due(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
                .unwrap()
        );
    }

    #[tokio::test]
    async fn cancelled_refresh_does_not_permanently_fence_predecessor() {
        let blocker = Arc::new(Notify::new());
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                valid_response("new-access", "new-refresh"),
            ))),
            block_first_token_request: Some(Arc::clone(&blocker)),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        let client = Client::new();
        let mut first = Box::pin(auth.refresh_now(&client, refresh_epoch));
        tokio::select! {
            () = state.first_token_request_started.notified() => {}
            result = &mut first => panic!("first refresh unexpectedly completed: {result:?}"),
        }
        drop(first);

        auth.refresh_now(&client, refresh_epoch).await.unwrap();
        blocker.notify_one();
        assert_eq!(auth.access_token(), "new-access");
        assert_eq!(auth.refresh_token(), "new-refresh");
        assert_eq!(state.token_request_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn non_bearer_token_type_is_accepted_like_teslamate() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                serde_json::json!({
                    "access_token": "new-access",
                    "refresh_token": "new-refresh",
                    "token_type": "MAC",
                    "expires_in": 1000,
                    "created_at": 1_700_000_000,
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
        assert_eq!(auth.access_token(), "new-access");
        assert_eq!(auth.refresh_token(), "new-refresh");
        assert_eq!(auth.token_type(), "MAC");
    }

    #[tokio::test]
    async fn missing_token_type_rotates_persists_and_defaults_to_bearer() {
        let response = serde_json::json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "expires_in": 1000,
            "created_at": 1_700_000_000,
        })
        .to_string();
        let state = MockState {
            token_response: Arc::new(Mutex::new((StatusCode::OK, response))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let persisted = Arc::new(Mutex::new(Vec::new()));
        let saved = Arc::clone(&persisted);
        auth.refresh_now_persisted(
            &Client::new(),
            refresh_epoch,
            move |access, refresh, _, _| {
                saved
                    .lock()
                    .unwrap()
                    .push((access.to_owned(), refresh.to_owned()));
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(auth.access_token(), "new-access");
        assert_eq!(auth.refresh_token(), "new-refresh");
        assert_eq!(auth.token_type(), "Bearer");
        assert_eq!(auth.next_refresh_at(), 1_700_000_750);
        assert_eq!(
            persisted.lock().unwrap().as_slice(),
            &[("new-access".to_owned(), "new-refresh".to_owned())]
        );
        assert_eq!(auth.retry_at(), None);
        assert_eq!(state.bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn null_token_type_rotates_persists_and_defaults_to_bearer() {
        let response = serde_json::json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "token_type": null,
            "expires_in": 1000,
        })
        .to_string();
        let state = MockState {
            token_response: Arc::new(Mutex::new((StatusCode::OK, response))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        auth.refresh_now_persisted(&Client::new(), refresh_epoch, |_, _, _, _| Ok(()))
            .await
            .unwrap();
        assert_eq!(auth.access_token(), "new-access");
        assert_eq!(auth.refresh_token(), "new-refresh");
        assert_eq!(auth.token_type(), "Bearer");
        assert_eq!(auth.next_refresh_at(), 1_700_000_750);
        assert_eq!(auth.retry_at(), None);
        assert_eq!(state.bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn malformed_success_response_retains_pair_and_retries() {
        for body in ["<html>not a token</html>", "not-json"] {
            let state = MockState {
                token_response: Arc::new(Mutex::new((StatusCode::OK, body.to_owned()))),
                ..MockState::default()
            };
            let (issuer, _task) = mock_server(state.clone()).await;
            let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
            let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
            assert_eq!(
                auth.refresh_now(&Client::new(), refresh_epoch)
                    .await
                    .unwrap_err(),
                LegacyAuthError::InvalidResponse
            );
            assert_eq!(auth.access_token(), "old-access");
            assert_eq!(auth.refresh_token(), "old-refresh");
            assert_eq!(
                auth.refresh_if_due(&Client::new(), refresh_epoch + Duration::from_secs(1))
                    .await
                    .unwrap_err(),
                LegacyAuthError::RefreshDeferred
            );
            assert_eq!(state.bodies.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn invalid_response_retains_pair_for_retry_without_redaction_leak() {
        let secret = "old-refresh-secret";
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                r#"{"access_token":"new-access","token_type":"Bearer","expires_in":1000,"created_at":1700000000}"#.to_owned(),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let mut auth = LegacyAuth::for_test(issuer, "old-access", secret);
        let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let error = auth
            .refresh_now(&Client::new(), refresh_epoch)
            .await
            .unwrap_err();
        assert_eq!(error, LegacyAuthError::InvalidResponse);
        assert_eq!(auth.refresh_token(), secret);
        assert_eq!(auth.retry_at(), Some(1_700_000_300));
        assert!(!format!("{auth:?}").contains(secret));
        assert!(!error.to_string().contains(secret));
        assert_eq!(
            auth.refresh_now(&Client::new(), refresh_epoch + Duration::from_secs(1))
                .await
                .unwrap_err(),
            LegacyAuthError::RefreshDeferred
        );
        assert_eq!(state.bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn non_200_response_retains_pair_and_retries() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::INTERNAL_SERVER_ERROR,
                "retry later".to_owned(),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let refresh_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        assert_eq!(
            auth.refresh_now(&Client::new(), refresh_epoch)
                .await
                .unwrap_err(),
            LegacyAuthError::HttpStatus(500)
        );
        assert_eq!(auth.access_token(), "old-access");
        assert_eq!(auth.refresh_token(), "old-refresh");
        assert_eq!(auth.retry_at(), Some(1_700_000_300));
        assert_eq!(
            auth.refresh_now(&Client::new(), refresh_epoch + Duration::from_secs(1))
                .await
                .unwrap_err(),
            LegacyAuthError::RefreshDeferred
        );
        assert_eq!(state.bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn token_endpoint_requires_status_exactly_200() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::CREATED,
                valid_response("new-access", "new-refresh"),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state).await;
        let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let receipt = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(
            auth.refresh_now(&Client::new(), receipt).await.unwrap_err(),
            LegacyAuthError::HttpStatus(201)
        );
        assert_eq!(auth.access_token(), "old-access");
        assert_eq!(auth.refresh_token(), "old-refresh");
        assert_eq!(auth.retry_at(), Some(1_700_000_300));
    }

    #[tokio::test]
    async fn bound_audit_force_bypasses_retry_twice_but_nonforce_remains_deferred() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::SERVICE_UNAVAILABLE,
                "refresh failed".to_owned(),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let client = Client::new();
        let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let first = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(
            auth.refresh_persisted_with_bound_audit(&client, first, true, |_, _, _, _| Ok(()))
                .await
                .unwrap_err(),
            LegacyAuthError::HttpStatus(503)
        );
        assert_eq!(auth.retry_at(), Some(1_700_000_300));
        let second = first + Duration::from_secs(1);
        assert_eq!(
            auth.refresh_persisted_with_bound_audit(&client, second, true, |_, _, _, _| Ok(()))
                .await
                .unwrap_err(),
            LegacyAuthError::HttpStatus(503)
        );
        assert_eq!(auth.retry_at(), Some(1_700_000_301));
        assert_eq!(state.token_request_count.load(Ordering::SeqCst), 2);
        assert_eq!(
            auth.refresh_persisted_with_bound_audit(
                &client,
                second + Duration::from_secs(1),
                false,
                |_, _, _, _| Ok(())
            )
            .await
            .unwrap_err(),
            LegacyAuthError::RefreshDeferred
        );
        assert_eq!(state.token_request_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_callback_force_retries_immediately_after_failure() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::SERVICE_UNAVAILABLE,
                "refresh failed".to_owned(),
            ))),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        let mut manager =
            crate::credentials::LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
        let first = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert!(matches!(
            manager.refresh_now(&Client::new(), first).await,
            Err(crate::credentials::LegacyAuthManagerError::Auth(
                LegacyAuthError::HttpStatus(503)
            ))
        ));
        assert!(matches!(
            manager
                .refresh_now(&Client::new(), first + Duration::from_secs(1))
                .await,
            Err(crate::credentials::LegacyAuthManagerError::Auth(
                LegacyAuthError::HttpStatus(503)
            ))
        ));
        assert_eq!(state.token_request_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn auth_client_rejects_redirect_without_replaying_refresh_body() {
        let state = MockState {
            token_response: Arc::new(Mutex::new((
                StatusCode::OK,
                valid_response("new-access", "new-refresh"),
            ))),
            token_redirects_remaining: Arc::new(Mutex::new(1)),
            token_redirect_location: Arc::new(Mutex::new("/oauth2/v3/redirect-capture".to_owned())),
            ..MockState::default()
        };
        let (issuer, _task) = mock_server(state.clone()).await;
        let owner =
            OwnerApi::for_fake_http(issuer.join("../../").unwrap(), Duration::from_millis(50))
                .unwrap();
        let mut auth = LegacyAuth::for_test(issuer.clone(), "old-access", "old-refresh");
        assert_eq!(
            auth.refresh_now(
                &owner.legacy_auth_http_client(),
                UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            )
            .await
            .unwrap_err(),
            LegacyAuthError::HttpStatus(307)
        );
        assert_eq!(state.token_request_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.redirect_capture_requests.load(Ordering::SeqCst),
            0,
            "redirect target must receive no request"
        );
        assert_eq!(
            state.redirect_capture_body_bytes.load(Ordering::SeqCst),
            0,
            "refresh body must not reach redirect target"
        );
        assert_eq!(
            state.redirect_capture_authorization.load(Ordering::SeqCst),
            0,
            "credential headers must not reach redirect target"
        );
    }

    #[tokio::test]
    async fn owner_api_401_is_one_wrapped_request_without_sync_refresh_or_retry() {
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
        let mut auth = LegacyAuth::for_test(issuer, "old-access", "old-refresh");
        auth.expires_at = 2_000_000_000;
        auth.next_refresh_at = 1_900_000_000;
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
        assert_eq!(state.token_request_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn account_unauthorized_fuse_is_shared_windowed_and_resettable() {
        let base = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut fuse = LegacyAuthFuse::default();
        for offset in 0..5 {
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
