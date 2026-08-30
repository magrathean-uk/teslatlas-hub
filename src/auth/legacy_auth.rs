// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(all(test, unix))]
use std::sync::atomic::{AtomicBool, Ordering};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

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

/// The single in-memory owner-auth authority. It is intentionally not cloneable.
///
/// ```compile_fail
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<teslatlas_hub::legacy_auth::LegacyAuth>();
/// ```
#[derive(PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct LegacyAuth {
    access_token: Zeroizing<String>,
    refresh_token: Zeroizing<String>,
    token_type: String,
    expires_at: i64,
    next_refresh_at: i64,
    #[zeroize(skip)]
    issuer: Url,
    #[zeroize(skip)]
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

async fn read_bounded_token_response(
    response: reqwest::Response,
) -> Result<Vec<u8>, LegacyAuthError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_TOKEN_RESPONSE_BYTES as u64)
    {
        return Err(LegacyAuthError::ResponseTooLarge);
    }

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| LegacyAuthError::Transport)?;
        let remaining = MAX_TOKEN_RESPONSE_BYTES
            .checked_sub(bytes.len())
            .ok_or(LegacyAuthError::ResponseTooLarge)?;
        if chunk.len() > remaining {
            return Err(LegacyAuthError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
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
        let access_token = Zeroizing::new(access_token.into());
        let refresh_token = Zeroizing::new(refresh_token.into());
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
        let bytes = match read_bounded_token_response(response).await {
            Ok(bytes) => bytes,
            Err(error) => return self.failed_refresh(receipt_epoch, error),
        };
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
        mut response: TokenResponse,
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
            access_token: Zeroizing::new(std::mem::take(&mut response.access_token)),
            refresh_token: Zeroizing::new(std::mem::take(&mut response.refresh_token)),
            token_type: std::mem::take(&mut response.token_type)
                .unwrap_or_else(|| "Bearer".to_owned()),
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
            access_token: Zeroizing::new(access_token.to_owned()),
            refresh_token: Zeroizing::new(refresh_token.to_owned()),
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

#[derive(Serialize)]
struct TokenRequest<'a> {
    grant_type: &'static str,
    scope: &'static str,
    client_id: &'static str,
    refresh_token: &'a str,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    token_type: Option<String>,
    expires_in: u64,
    #[serde(rename = "created_at")]
    _created_at: Option<i64>,
}

struct ValidatedTokens {
    access_token: Zeroizing<String>,
    refresh_token: Zeroizing<String>,
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
#[path = "legacy_auth/tests.rs"]
mod tests;
