use std::{
    fmt, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::{
    owner_api::{OwnerApiBase, OwnerApiOptions},
    protocol::Sha256Digest,
    teslamate::ReadOnlySource,
    teslamate_reader::TeslaMateReadLimits,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubConfig {
    pub data_dir: PathBuf,
    pub bind: SocketAddr,
    #[serde(default)]
    pub tls: Option<TlsListenerConfig>,
    #[serde(default)]
    pub collector: CollectorConfig,
    #[serde(default)]
    pub geocoder: GeocoderConfig,
    #[serde(default)]
    pub teslamate: TeslaMateConfig,
    #[serde(default)]
    pub terrain: TerrainConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainConfig {
    #[serde(default = "default_terrain_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub cache_dir: Option<PathBuf>,
    #[serde(default = "default_terrain_min_free_bytes")]
    pub min_free_bytes: u64,
    #[serde(default = "default_terrain_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_terrain_read_timeout_seconds")]
    pub read_timeout_seconds: u64,
}

const fn default_terrain_enabled() -> bool {
    true
}

fn default_terrain_min_free_bytes() -> u64 {
    128 * 1024 * 1024
}
fn default_terrain_connect_timeout_seconds() -> u64 {
    15
}
fn default_terrain_read_timeout_seconds() -> u64 {
    60
}

impl Default for TerrainConfig {
    fn default() -> Self {
        Self {
            enabled: default_terrain_enabled(),
            cache_dir: None,
            min_free_bytes: default_terrain_min_free_bytes(),
            connect_timeout_seconds: default_terrain_connect_timeout_seconds(),
            read_timeout_seconds: default_terrain_read_timeout_seconds(),
        }
    }
}

impl TerrainConfig {
    pub fn resolved_cache_dir(&self, data_dir: &Path) -> PathBuf {
        if let Some(path) = &self.cache_dir {
            return path.clone();
        }
        #[cfg(target_os = "macos")]
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Library/Caches/TeslatlasHub/terrain");
        }
        if let Ok(cache) = std::env::var("XDG_CACHE_HOME") {
            return PathBuf::from(cache).join("teslatlas-hub/terrain");
        }
        data_dir.join("cache/terrain")
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.min_free_bytes == 0
            || self.connect_timeout_seconds == 0
            || self.read_timeout_seconds == 0
        {
            return Err(ConfigError::InvalidTerrainConfig);
        }
        Ok(())
    }
}

/// TLS is mandatory for a non-loopback Hub listener. Device pairing and bearer
/// credentials are never sent over plaintext HTTP.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsListenerConfig {
    pub certificate_path: PathBuf,
    pub private_key_path: PathBuf,
    pub public_url: String,
}

impl TlsListenerConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if !self.certificate_path.is_absolute() || !self.private_key_path.is_absolute() {
            return Err(ConfigError::TlsPathMustBeAbsolute);
        }
        if self.certificate_path == self.private_key_path {
            return Err(ConfigError::TlsPathsMustDiffer);
        }
        let endpoint =
            Url::parse(&self.public_url).map_err(|_| ConfigError::InvalidTlsPublicUrl)?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || endpoint.username() != ""
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/"
        {
            return Err(ConfigError::InvalidTlsPublicUrl);
        }
        Ok(())
    }
}

/// Explicit settings for a manual or supervised legacy owner-token read.
///
/// The default is Tesla's legacy Owner API. A loopback endpoint may override it
/// for local tests. The URL cannot contain credentials or query parameters.
/// Supervised collection runs by default. Set `interval_seconds = 0` to
/// disable it explicitly.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectorConfig {
    #[serde(default = "default_owner_api_base_url")]
    pub owner_api_base_url: Option<String>,
    #[serde(default = "default_owner_api_timeout_seconds")]
    pub request_timeout_seconds: u64,
    /// Positive interval for `collect-supervised`; explicit zero disables it.
    #[serde(default = "default_collector_interval_seconds")]
    pub interval_seconds: u64,
    #[serde(default = "default_collector_max_backoff_seconds")]
    pub max_backoff_seconds: u64,
    #[serde(default = "default_driving_poll_milliseconds")]
    pub driving_poll_milliseconds: u64,
    #[serde(default = "default_charging_poll_seconds")]
    pub charging_poll_seconds: u64,
    #[serde(default = "default_online_poll_seconds")]
    pub online_poll_seconds: u64,
    #[serde(default = "default_sleeping_poll_seconds")]
    pub sleeping_poll_seconds: u64,
    #[serde(default = "default_offline_drive_timeout_seconds")]
    pub offline_drive_timeout_seconds: u64,
    #[serde(default = "default_idle_suspend_after_seconds")]
    pub idle_suspend_after_seconds: u64,
    #[serde(default = "default_suspended_poll_seconds")]
    pub suspended_poll_seconds: u64,
    #[serde(default = "default_updating_poll_seconds")]
    pub updating_poll_seconds: u64,
    /// Optional websocket endpoint override. Production overrides must use
    /// wss; ws is accepted only for loopback test endpoints.
    #[serde(default)]
    pub stream_endpoint_override: Option<String>,
    /// Region for provider tokens; absent means derive from the API host.
    #[serde(default)]
    pub stream_region: Option<crate::tesla_stream::StreamRegion>,
    #[serde(default = "default_stream_health_timeout_seconds")]
    pub stream_health_timeout_seconds: u64,
    /// Enables explicit legacy owner-auth refresh calls. No secret is read
    /// from configuration, and this mode is never used for Fleet TOKEN auth.
    #[serde(default)]
    pub legacy_auth: LegacyAuthConfig,
}

impl fmt::Debug for CollectorConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CollectorConfig")
            .field(
                "owner_api_base_url",
                &self.owner_api_base_url.as_ref().map(|_| "[redacted]"),
            )
            .field("request_timeout_seconds", &self.request_timeout_seconds)
            .field("interval_seconds", &self.interval_seconds)
            .field("max_backoff_seconds", &self.max_backoff_seconds)
            .field("driving_poll_milliseconds", &self.driving_poll_milliseconds)
            .field("charging_poll_seconds", &self.charging_poll_seconds)
            .field("online_poll_seconds", &self.online_poll_seconds)
            .field("sleeping_poll_seconds", &self.sleeping_poll_seconds)
            .field(
                "offline_drive_timeout_seconds",
                &self.offline_drive_timeout_seconds,
            )
            .field(
                "idle_suspend_after_seconds",
                &self.idle_suspend_after_seconds,
            )
            .field("suspended_poll_seconds", &self.suspended_poll_seconds)
            .field("updating_poll_seconds", &self.updating_poll_seconds)
            .field(
                "stream_endpoint_override",
                &self.stream_endpoint_override.as_ref().map(|_| "[redacted]"),
            )
            .field("stream_region", &self.stream_region)
            .field(
                "stream_health_timeout_seconds",
                &self.stream_health_timeout_seconds,
            )
            .field("legacy_auth", &self.legacy_auth)
            .finish()
    }
}

const fn default_owner_api_timeout_seconds() -> u64 {
    20
}

fn default_owner_api_base_url() -> Option<String> {
    Some("https://owner-api.teslamotors.com/".to_owned())
}

const fn default_collector_interval_seconds() -> u64 {
    60
}

const fn default_collector_max_backoff_seconds() -> u64 {
    900
}

const fn default_driving_poll_milliseconds() -> u64 {
    2_500
}

const fn default_charging_poll_seconds() -> u64 {
    5
}

const fn default_online_poll_seconds() -> u64 {
    60
}

const fn default_sleeping_poll_seconds() -> u64 {
    30
}

const fn default_offline_drive_timeout_seconds() -> u64 {
    15 * 60
}

const fn default_idle_suspend_after_seconds() -> u64 {
    15 * 60
}

const fn default_suspended_poll_seconds() -> u64 {
    21 * 60
}

const fn default_updating_poll_seconds() -> u64 {
    15
}

const fn default_stream_health_timeout_seconds() -> u64 {
    30
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            owner_api_base_url: default_owner_api_base_url(),
            request_timeout_seconds: default_owner_api_timeout_seconds(),
            interval_seconds: default_collector_interval_seconds(),
            max_backoff_seconds: default_collector_max_backoff_seconds(),
            driving_poll_milliseconds: default_driving_poll_milliseconds(),
            charging_poll_seconds: default_charging_poll_seconds(),
            online_poll_seconds: default_online_poll_seconds(),
            sleeping_poll_seconds: default_sleeping_poll_seconds(),
            offline_drive_timeout_seconds: default_offline_drive_timeout_seconds(),
            idle_suspend_after_seconds: default_idle_suspend_after_seconds(),
            suspended_poll_seconds: default_suspended_poll_seconds(),
            updating_poll_seconds: default_updating_poll_seconds(),
            stream_endpoint_override: None,
            stream_region: None,
            stream_health_timeout_seconds: default_stream_health_timeout_seconds(),
            legacy_auth: LegacyAuthConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyAuthConfig {
    #[serde(default)]
    pub enabled: bool,
}

impl Default for LegacyAuthConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeocoderConfig {
    #[serde(default = "default_geocoder_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default = "default_geocoder_language")]
    pub language: String,
    #[serde(default = "default_geocoder_timeout_seconds")]
    pub timeout_seconds: u64,
}

impl Default for GeocoderConfig {
    fn default() -> Self {
        Self {
            enabled: default_geocoder_enabled(),
            endpoint: None,
            language: default_geocoder_language(),
            timeout_seconds: default_geocoder_timeout_seconds(),
        }
    }
}

const fn default_geocoder_enabled() -> bool {
    true
}

impl GeocoderConfig {
    pub(crate) fn endpoint_url(&self, test_mode: bool) -> Result<Url, ConfigError> {
        let raw = self
            .endpoint
            .as_deref()
            .unwrap_or(crate::geocoder::DEFAULT_NOMINATIM_ENDPOINT);
        let mut endpoint = Url::parse(raw).map_err(|_| ConfigError::InvalidGeocoderEndpoint)?;
        if endpoint.host_str().is_none()
            || !matches!(endpoint.scheme(), "https" | "http")
            || (!test_mode && endpoint.scheme() != "https")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ConfigError::InvalidGeocoderEndpoint);
        }
        if !endpoint.path().ends_with('/') {
            endpoint.set_path(&format!("{}/", endpoint.path()));
        }
        Ok(endpoint)
    }

    pub(crate) fn timeout(&self) -> Result<Duration, ConfigError> {
        if self.timeout_seconds == 0 || self.timeout_seconds > 120 {
            return Err(ConfigError::InvalidGeocoderTimeout);
        }
        Ok(Duration::from_secs(self.timeout_seconds))
    }

    pub(crate) fn validated_language(&self) -> Result<String, ConfigError> {
        if self.language.is_empty()
            || self.language.len() > 64
            || self.language.chars().any(char::is_control)
        {
            return Err(ConfigError::InvalidGeocoderLanguage);
        }
        Ok(self.language.clone())
    }
}

fn default_geocoder_language() -> String {
    "en".to_owned()
}

const fn default_geocoder_timeout_seconds() -> u64 {
    30
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectorCadence {
    pub driving: Duration,
    pub charging: Duration,
    pub online: Duration,
    pub sleeping: Duration,
    pub offline_drive_timeout: Duration,
    pub idle_suspend_after: Duration,
    pub suspended: Duration,
    pub updating: Duration,
    pub stream_health_timeout: Duration,
    pub maximum_backoff: Duration,
}

impl CollectorConfig {
    pub fn owner_api_options(&self) -> Result<OwnerApiOptions, ConfigError> {
        self.owner_api_options_for_region(crate::tesla_stream::StreamRegion::Global)
    }

    /// Resolve the legacy Owner API endpoint for the token's region. The
    /// canonical global default is provider-selected for China tokens; an
    /// explicit custom endpoint always wins.
    pub fn owner_api_options_for_region(
        &self,
        region: crate::tesla_stream::StreamRegion,
    ) -> Result<OwnerApiOptions, ConfigError> {
        let base_url = self
            .owner_api_base_url
            .as_deref()
            .ok_or(ConfigError::OwnerApiBaseRequired)?;
        let mut base_url =
            OwnerApiBase::parse(base_url).map_err(|_| ConfigError::InvalidOwnerApiBase)?;
        if region == crate::tesla_stream::StreamRegion::China
            && base_url
                == OwnerApiBase::parse("https://owner-api.teslamotors.com/")
                    .map_err(|_| ConfigError::InvalidOwnerApiBase)?
        {
            base_url = OwnerApiBase::parse("https://owner-api.vn.cloud.tesla.cn/")
                .map_err(|_| ConfigError::InvalidOwnerApiBase)?;
        }
        let request_timeout = Duration::from_secs(self.request_timeout_seconds);
        if request_timeout.is_zero() {
            return Err(ConfigError::InvalidOwnerApiTimeout);
        }
        Ok(OwnerApiOptions::new(base_url, request_timeout))
    }

    pub fn supervised_interval(&self) -> Result<Duration, ConfigError> {
        if self.interval_seconds == 0 {
            return Err(ConfigError::SupervisedIntervalRequired);
        }
        Ok(Duration::from_secs(self.interval_seconds))
    }

    pub fn cadence(&self) -> Result<CollectorCadence, ConfigError> {
        let cadence = CollectorCadence {
            driving: Duration::from_millis(self.driving_poll_milliseconds),
            charging: Duration::from_secs(self.charging_poll_seconds),
            online: Duration::from_secs(self.online_poll_seconds),
            sleeping: Duration::from_secs(self.sleeping_poll_seconds),
            offline_drive_timeout: Duration::from_secs(self.offline_drive_timeout_seconds),
            idle_suspend_after: Duration::from_secs(self.idle_suspend_after_seconds),
            suspended: Duration::from_secs(self.suspended_poll_seconds),
            updating: Duration::from_secs(self.updating_poll_seconds),
            stream_health_timeout: Duration::from_secs(self.stream_health_timeout_seconds),
            maximum_backoff: Duration::from_secs(self.max_backoff_seconds),
        };
        if cadence.driving.is_zero()
            || cadence.charging.is_zero()
            || cadence.online.is_zero()
            || cadence.sleeping.is_zero()
            || cadence.offline_drive_timeout.is_zero()
            || cadence.idle_suspend_after.is_zero()
            || cadence.suspended.is_zero()
            || cadence.updating.is_zero()
            || cadence.stream_health_timeout.is_zero()
            || cadence.maximum_backoff.is_zero()
        {
            return Err(ConfigError::InvalidCollectorCadence);
        }
        Ok(cadence)
    }

    pub fn stream_endpoint(
        &self,
        region: crate::tesla_stream::StreamRegion,
    ) -> Result<String, ConfigError> {
        if let Some(override_url) = &self.stream_endpoint_override {
            crate::tesla_stream::validate_endpoint_override(override_url)
                .map_err(|_| ConfigError::InvalidStreamEndpoint)?;
            return Ok(override_url.clone());
        }
        Ok(crate::tesla_stream::streaming_endpoint(region).to_owned())
    }

    pub fn stream_region(&self) -> Result<crate::tesla_stream::StreamRegion, ConfigError> {
        if let Some(region) = self.stream_region {
            return Ok(region);
        }
        let base = self
            .owner_api_base_url
            .as_deref()
            .ok_or(ConfigError::StreamRegionRequired)?;
        OwnerApiBase::parse(base)
            .map_err(|_| ConfigError::InvalidOwnerApiBase)?
            .stream_region()
            .ok_or(ConfigError::StreamRegionRequired)
    }
}

/// Opt-in TeslaMate import source. The endpoint excludes its password and the
/// source key is a durable owner label, not an endpoint alias.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeslaMateConfig {
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub source_key: Option<String>,
    #[serde(default = "default_teslamate_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_teslamate_copy_statement_timeout_seconds")]
    pub copy_statement_timeout_seconds: u64,
    #[serde(default = "default_teslamate_page_size")]
    pub page_size: i32,
    #[serde(default = "default_teslamate_maximum_rows")]
    pub maximum_rows: usize,
    #[serde(default = "default_teslamate_maximum_stage_bytes")]
    pub maximum_stage_bytes: u64,
    #[serde(default = "default_teslamate_minimum_free_bytes")]
    pub minimum_free_bytes: u64,
    #[serde(default = "default_teslamate_parallel_copy_lanes")]
    pub parallel_copy_lanes: usize,
    #[serde(default)]
    pub performance_profile: PerformanceProfileConfig,
}

impl fmt::Debug for TeslaMateConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TeslaMateConfig")
            .field(
                "source_url",
                &self.source_url.as_ref().map(|_| "[redacted]"),
            )
            .field("source_key", &self.source_key)
            .field("connect_timeout_seconds", &self.connect_timeout_seconds)
            .field(
                "copy_statement_timeout_seconds",
                &self.copy_statement_timeout_seconds,
            )
            .field("page_size", &self.page_size)
            .field("maximum_rows", &self.maximum_rows)
            .field("maximum_stage_bytes", &self.maximum_stage_bytes)
            .field("minimum_free_bytes", &self.minimum_free_bytes)
            .field("parallel_copy_lanes", &self.parallel_copy_lanes)
            .field("performance_profile", &self.performance_profile)
            .finish()
    }
}

/// Conservative host-local tuning for the TeslaMate import reader.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceProfileConfig {
    #[serde(default = "default_performance_profile_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub max_parallel_copy_lanes: Option<usize>,
}

const fn default_performance_profile_enabled() -> bool {
    true
}

impl Default for PerformanceProfileConfig {
    fn default() -> Self {
        Self {
            enabled: default_performance_profile_enabled(),
            max_parallel_copy_lanes: None,
        }
    }
}

const fn default_teslamate_connect_timeout_seconds() -> u64 {
    10
}

const fn default_teslamate_copy_statement_timeout_seconds() -> u64 {
    2 * 60 * 60
}

const fn default_teslamate_page_size() -> i32 {
    10_000
}

const fn default_teslamate_maximum_rows() -> usize {
    20_000_000
}

const fn default_teslamate_maximum_stage_bytes() -> u64 {
    16 * 1024 * 1024 * 1024
}

const fn default_teslamate_minimum_free_bytes() -> u64 {
    512 * 1024 * 1024
}

const fn default_teslamate_parallel_copy_lanes() -> usize {
    4
}

impl Default for TeslaMateConfig {
    fn default() -> Self {
        Self {
            source_url: None,
            source_key: None,
            connect_timeout_seconds: default_teslamate_connect_timeout_seconds(),
            copy_statement_timeout_seconds: default_teslamate_copy_statement_timeout_seconds(),
            page_size: default_teslamate_page_size(),
            maximum_rows: default_teslamate_maximum_rows(),
            maximum_stage_bytes: default_teslamate_maximum_stage_bytes(),
            minimum_free_bytes: default_teslamate_minimum_free_bytes(),
            parallel_copy_lanes: default_teslamate_parallel_copy_lanes(),
            performance_profile: PerformanceProfileConfig::default(),
        }
    }
}

#[derive(Clone)]
pub struct TeslaMateImportConfig {
    pub source: ReadOnlySource,
    pub source_key: String,
    pub limits: TeslaMateReadLimits,
    pub performance_profile: PerformanceProfileConfig,
}

impl fmt::Debug for TeslaMateImportConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TeslaMateImportConfig")
            .field("source", &self.source)
            .field("source_key", &self.source_key)
            .field("limits", &self.limits)
            .field("performance_profile", &self.performance_profile)
            .finish()
    }
}

impl TeslaMateConfig {
    /// Build and validate source-read limits for both configured and CLI
    /// supplied TeslaMate sources. Source URL/key are intentionally separate.
    pub fn read_limits(&self) -> Result<TeslaMateReadLimits, ConfigError> {
        let limits = TeslaMateReadLimits {
            connect_timeout: Duration::from_secs(self.connect_timeout_seconds),
            copy_statement_timeout: Duration::from_secs(self.copy_statement_timeout_seconds),
            page_size: self.page_size,
            maximum_rows: self.maximum_rows,
            maximum_stage_bytes: self.maximum_stage_bytes,
            minimum_free_bytes: self.minimum_free_bytes,
            parallel_copy_lanes: self.parallel_copy_lanes,
        };
        limits
            .validate()
            .map_err(|_| ConfigError::InvalidTeslaMateLimits)?;

        let maximum = self.performance_profile.max_parallel_copy_lanes;
        if let Some(maximum) = maximum {
            let mut validation_probe = limits;
            validation_probe.parallel_copy_lanes = maximum;
            validation_probe
                .validate()
                .map_err(|_| ConfigError::InvalidTeslaMateLimits)?;
        }
        if !self.performance_profile.enabled {
            return Ok(limits);
        }

        let mut effective = limits;
        if let Some(maximum) = maximum {
            effective.parallel_copy_lanes = effective.parallel_copy_lanes.min(maximum);
        }
        Ok(effective)
    }

    pub fn import_config(&self) -> Result<TeslaMateImportConfig, ConfigError> {
        let source_url = self
            .source_url
            .as_deref()
            .ok_or(ConfigError::TeslaMateSourceRequired)?;
        let source =
            ReadOnlySource::parse(source_url).map_err(|_| ConfigError::InvalidTeslaMateSource)?;
        let source_key = self
            .source_key
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or(ConfigError::TeslaMateSourceKeyRequired)?;
        if source_key.contains("://")
            || source_key.contains('@')
            || source_key.chars().any(char::is_control)
        {
            return Err(ConfigError::InvalidTeslaMateSourceKey);
        }
        let limits = self.read_limits()?;
        Ok(TeslaMateImportConfig {
            source,
            source_key: source_key.to_owned(),
            limits,
            performance_profile: self.performance_profile.clone(),
        })
    }
}

impl HubConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        Self::load_with_digest(path).map(|(config, _digest)| config)
    }

    /// Reads, parses, validates, and hashes one exact configuration snapshot.
    /// The returned digest therefore identifies the bytes that produced the
    /// in-memory configuration rather than a second, racy filesystem read.
    pub fn load_with_digest(path: impl AsRef<Path>) -> Result<(Self, Sha256Digest), ConfigError> {
        let path = path.as_ref();
        let source = fs::read(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_exact_bytes(&source)
    }

    /// Parses and validates the one exact configuration byte sequence supplied
    /// by the caller.  This is deliberately shared with `load_with_digest` so
    /// callers that hold an already-admitted descriptor do not need to reopen
    /// a pathname merely to preserve ordinary configuration semantics.
    pub fn from_exact_bytes(source: &[u8]) -> Result<(Self, Sha256Digest), ConfigError> {
        let source = std::str::from_utf8(source)
            .map_err(|_| ConfigError::Invalid("configuration must be UTF-8"))?;
        let digest = Sha256Digest::of_bytes(source.as_bytes());
        let config: Self = toml::from_str(source).map_err(ConfigError::Parse)?;
        config.validate()?;
        Ok((config, digest))
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.data_dir.as_os_str().is_empty() {
            return Err(ConfigError::Invalid("data_dir is required"));
        }
        if !self.data_dir.is_absolute() {
            return Err(ConfigError::Invalid("data_dir must be absolute"));
        }
        if self.bind.port() == 0 {
            return Err(ConfigError::Invalid("bind port must not be zero"));
        }
        if !self.bind.ip().is_loopback() && self.tls.is_none() {
            return Err(ConfigError::NonLoopbackBind);
        }
        if let Some(tls) = &self.tls {
            tls.validate()?;
        }
        if self.collector.request_timeout_seconds == 0 {
            return Err(ConfigError::InvalidOwnerApiTimeout);
        }
        self.geocoder.endpoint_url(false)?;
        self.geocoder.timeout()?;
        self.geocoder.validated_language()?;
        self.terrain.validate()?;
        self.collector.cadence()?;
        if let Some(base_url) = self.collector.owner_api_base_url.as_deref() {
            OwnerApiBase::parse(base_url).map_err(|_| ConfigError::InvalidOwnerApiBase)?;
        }
        if self.collector.legacy_auth.enabled && self.collector.owner_api_base_url.is_none() {
            return Err(ConfigError::OwnerApiBaseRequired);
        }
        if let Some(endpoint) = self.collector.stream_endpoint_override.as_deref() {
            crate::tesla_stream::validate_endpoint_override(endpoint)
                .map_err(|_| ConfigError::InvalidStreamEndpoint)?;
        }
        let source_set = self.teslamate.source_url.is_some();
        let source_key_set = self.teslamate.source_key.is_some();
        if source_set != source_key_set {
            return Err(ConfigError::TeslaMatePartialConfiguration);
        }
        if source_set {
            self.teslamate.import_config()?;
        }
        Ok(())
    }

    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("hub.sqlite")
    }

    pub fn packs_dir(&self) -> PathBuf {
        self.data_dir.join("packs")
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read configuration {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid configuration: {0}")]
    Parse(toml::de::Error),
    #[error("invalid configuration: {0}")]
    Invalid(&'static str),
    #[error("collector owner API base URL is required for manual collection")]
    OwnerApiBaseRequired,
    #[error("collector owner API base URL is invalid")]
    InvalidOwnerApiBase,
    #[error("collector owner API timeout must be greater than zero")]
    InvalidOwnerApiTimeout,
    #[error("stream region must be explicit or derivable from the owner API host")]
    StreamRegionRequired,
    #[error("stream endpoint override is invalid or unsafe")]
    InvalidStreamEndpoint,
    #[error("geocoder endpoint is invalid or unsafe")]
    InvalidGeocoderEndpoint,
    #[error("geocoder language is invalid")]
    InvalidGeocoderLanguage,
    #[error("geocoder timeout is invalid")]
    InvalidGeocoderTimeout,
    #[error("terrain cache configuration is invalid")]
    InvalidTerrainConfig,
    #[error("supervised collector requires an explicit interval_seconds")]
    SupervisedIntervalRequired,
    #[error("collector cadence values must be greater than zero")]
    InvalidCollectorCadence,
    #[error("non-loopback bind requires configured TLS")]
    NonLoopbackBind,
    #[error("TLS certificate and private-key paths must be absolute")]
    TlsPathMustBeAbsolute,
    #[error("TLS certificate and private-key paths must differ")]
    TlsPathsMustDiffer,
    #[error("TLS public_url must be an origin-only HTTPS URL without credentials")]
    InvalidTlsPublicUrl,
    #[error("TeslaMate source URL and source key must be configured together")]
    TeslaMatePartialConfiguration,
    #[error("TeslaMate source URL is required for import")]
    TeslaMateSourceRequired,
    #[error("TeslaMate source URL is invalid")]
    InvalidTeslaMateSource,
    #[error("TeslaMate source key is required for import")]
    TeslaMateSourceKeyRequired,
    #[error("TeslaMate source key must be an opaque non-secret label")]
    InvalidTeslaMateSourceKey,
    #[error("TeslaMate import limits are invalid")]
    InvalidTeslaMateLimits,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_configuration_keys() {
        let error = toml::from_str::<HubConfig>(
            "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\nunknown = true",
        )
        .expect_err("unknown config must fail");
        assert!(error.to_string().contains("unknown"));
    }

    #[test]
    fn derives_storage_paths() {
        let config: HubConfig =
            toml::from_str("data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'")
                .expect("valid config");
        assert_eq!(
            config.database_path(),
            PathBuf::from("/var/lib/teslatlas/hub.sqlite")
        );
        assert_eq!(
            config.packs_dir(),
            PathBuf::from("/var/lib/teslatlas/packs")
        );
    }

    #[test]
    fn rejects_relative_data_directory() {
        let error =
            HubConfig::from_exact_bytes(b"data_dir = 'relative/hub'\nbind = '127.0.0.1:8080'\n")
                .expect_err("relative data directory");
        assert!(error.to_string().contains("data_dir must be absolute"));
    }

    #[test]
    fn collector_uses_the_legacy_owner_api_by_default() {
        let config: HubConfig =
            toml::from_str("data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'")
                .expect("valid config");
        assert_eq!(
            config
                .collector
                .owner_api_options()
                .expect("default owner API")
                .base_url
                .as_str(),
            "https://owner-api.teslamotors.com/"
        );
    }

    #[test]
    fn default_collector_cadence_matches_teslamate_v4() {
        let config = CollectorConfig::default();
        assert_eq!(
            config.supervised_interval().expect("default interval"),
            Duration::from_secs(60)
        );
        let cadence = config.cadence().expect("default cadence");
        assert_eq!(cadence.driving, Duration::from_millis(2_500));
        assert_eq!(cadence.charging, Duration::from_secs(5));
        assert_eq!(cadence.online, Duration::from_secs(60));
    }

    #[test]
    fn teslamate_read_limits_use_configured_stage_cap() {
        let mut config = TeslaMateConfig::default();
        assert_eq!(config.maximum_stage_bytes, 16 * 1024 * 1024 * 1024);
        assert_eq!(config.page_size, 10_000);
        config.maximum_stage_bytes = 8 * 1024 * 1024 * 1024;
        assert_eq!(
            config
                .read_limits()
                .expect("valid read limits")
                .maximum_stage_bytes,
            8 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn enrichment_defaults_on() {
        assert!(TerrainConfig::default().enabled);
        assert!(GeocoderConfig::default().enabled);
    }

    #[test]
    fn china_region_replaces_only_the_canonical_global_owner_api_default() {
        let mut config = CollectorConfig::default();
        assert_eq!(
            config
                .owner_api_options_for_region(crate::tesla_stream::StreamRegion::China)
                .expect("china owner API")
                .base_url
                .as_str(),
            "https://owner-api.vn.cloud.tesla.cn/"
        );
        config.owner_api_base_url = Some("https://owner.example.test/".to_owned());
        assert_eq!(
            config
                .owner_api_options_for_region(crate::tesla_stream::StreamRegion::China)
                .expect("custom owner API")
                .base_url
                .as_str(),
            "https://owner.example.test/"
        );
    }

    #[test]
    fn teslamate_read_limits_only_apply_a_non_raising_parallel_lane_cap() {
        let mut config = TeslaMateConfig::default();
        config.performance_profile.max_parallel_copy_lanes = Some(8);
        assert_eq!(
            config
                .read_limits()
                .expect("valid read limits")
                .parallel_copy_lanes,
            4,
            "an upper bound must not raise the configured lane count"
        );

        config.performance_profile.max_parallel_copy_lanes = Some(2);
        assert_eq!(
            config
                .read_limits()
                .expect("valid read limits")
                .parallel_copy_lanes,
            2,
            "an enabled profile may lower the configured lane count"
        );

        config.performance_profile.enabled = false;
        config.performance_profile.max_parallel_copy_lanes = Some(1);
        assert_eq!(
            config
                .read_limits()
                .expect("valid disabled profile")
                .parallel_copy_lanes,
            4,
            "a disabled profile must preserve the configured lane count"
        );

        config.performance_profile.enabled = true;
        config.performance_profile.max_parallel_copy_lanes = Some(0);
        assert!(matches!(
            config.read_limits(),
            Err(ConfigError::InvalidTeslaMateLimits)
        ));
    }

    #[test]
    fn explicit_zero_disables_the_default_supervised_collector() {
        let config: HubConfig = toml::from_str(
            "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
             [collector]\ninterval_seconds = 0\n",
        )
        .expect("config");
        assert!(matches!(
            config.collector.supervised_interval(),
            Err(ConfigError::SupervisedIntervalRequired)
        ));
    }

    #[test]
    fn loaded_config_digest_binds_every_exact_source_byte() {
        let temporary = tempfile::tempdir().expect("temporary config root");
        let path = temporary.path().join("config.toml");
        let first = "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
                     [collector]\nowner_api_base_url = 'https://owner.example.test'\n";
        fs::write(&path, first).expect("write first config");
        let (_config, first_digest) =
            HubConfig::load_with_digest(&path).expect("load first config snapshot");
        assert_eq!(first_digest, Sha256Digest::of_bytes(first.as_bytes()));

        let second = first.replace("owner.example.test", "other.example.test");
        fs::write(&path, &second).expect("write changed config");
        let (_config, second_digest) =
            HubConfig::load_with_digest(&path).expect("load second config snapshot");
        assert_eq!(second_digest, Sha256Digest::of_bytes(second.as_bytes()));
        assert_ne!(first_digest, second_digest);
    }

    #[test]
    fn stream_health_timeout_is_configured_and_must_be_positive() {
        let config = CollectorConfig::default();
        assert_eq!(
            config.cadence().unwrap().stream_health_timeout,
            Duration::from_secs(30)
        );
        let mut invalid = config;
        invalid.stream_health_timeout_seconds = 0;
        assert!(matches!(
            invalid.cadence(),
            Err(ConfigError::InvalidCollectorCadence)
        ));
    }

    #[test]
    fn collector_rejects_insecure_or_secret_bearing_bases() {
        for base in [
            "http://owner.example.test",
            "https://token@owner.example.test",
            "https://owner.example.test/?token=secret",
        ] {
            let config = format!(
                "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
                 [collector]\nowner_api_base_url = '{base}'"
            );
            let parsed = toml::from_str::<HubConfig>(&config).expect("parse before validation");
            assert!(parsed.validate().is_err());
        }
    }

    #[test]
    fn collector_debug_redacts_owner_and_stream_endpoint_values() {
        let mut collector = CollectorConfig {
            owner_api_base_url: Some("https://owner.example/access-secret/".to_owned()),
            stream_endpoint_override: Some("wss://stream.example/refresh-secret/".to_owned()),
            ..CollectorConfig::default()
        };
        let rendered = format!("{collector:?}");
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
        assert_eq!(rendered.matches("[redacted]").count(), 2);

        collector.owner_api_base_url = Some("https://owner.example/?access-secret=1".to_owned());
        collector.stream_endpoint_override =
            Some("wss://stream.example/?refresh-secret=1".to_owned());
        let owner_error = collector.owner_api_options().unwrap_err();
        let stream_error = collector
            .stream_endpoint(crate::tesla_stream::StreamRegion::Global)
            .unwrap_err();
        for rendered in [
            format!("{owner_error} {owner_error:?}"),
            format!("{stream_error} {stream_error:?}"),
        ] {
            assert!(!rendered.contains("access-secret"));
            assert!(!rendered.contains("refresh-secret"));
        }
    }

    #[test]
    fn teslamate_debug_redacts_nested_plain_and_encoded_userinfo() {
        let accepted: HubConfig = toml::from_str(
            "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
             [teslamate]\n\
             source_url = 'postgresql://plain-user-marker%40encoded-user-marker@db.example:5433/teslamate'\n\
             source_key = 'garage-teslamate'",
        )
        .expect("parse accepted source configuration");
        accepted.validate().expect("validate accepted source");
        let import = accepted
            .teslamate
            .import_config()
            .expect("build import configuration");

        for rendered in [
            format!("{:?}", accepted.teslamate),
            format!("{accepted:?}"),
            format!("{import:?}"),
        ] {
            for marker in ["plain-user-marker", "encoded-user-marker", "%40"] {
                assert!(!rendered.contains(marker), "leaked marker {marker}");
            }
        }

        let import_debug = format!("{import:?}");
        assert!(import_debug.contains("[redacted]"));
        assert!(import_debug.contains("db.example"));
        assert!(import_debug.contains("5433"));
        assert!(import_debug.contains("teslamate"));
        assert!(import_debug.contains("garage-teslamate"));

        let rejected: HubConfig = toml::from_str(
            "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
             [teslamate]\n\
             source_url = 'postgresql://password-user-marker:percent%2Dencoded%2Dpassword@db.example/teslamate'\n\
             source_key = 'garage-teslamate'",
        )
        .expect("parse rejected source configuration before validation");
        let error = rejected
            .teslamate
            .import_config()
            .expect_err("embedded password must be rejected");
        for rendered in [
            format!("{:?}", rejected.teslamate),
            format!("{rejected:?}"),
            format!("{error} {error:?}"),
        ] {
            for marker in [
                "password-user-marker",
                "percent%2Dencoded%2Dpassword",
                "percent-encoded-password",
            ] {
                assert!(!rendered.contains(marker), "leaked marker {marker}");
            }
        }
    }

    #[test]
    fn rejects_network_exposure_without_tls() {
        let config: HubConfig =
            toml::from_str("data_dir = '/var/lib/teslatlas'\nbind = '0.0.0.0:8080'")
                .expect("parse configuration");
        assert!(matches!(
            config.validate(),
            Err(ConfigError::NonLoopbackBind)
        ));
    }

    #[test]
    fn permits_remote_tls_only_with_safe_public_origin() {
        let config: HubConfig = toml::from_str(
            "data_dir = '/var/lib/teslatlas'\nbind = '0.0.0.0:8443'\n\
             [tls]\ncertificate_path = '/etc/teslatlas/tls/cert.pem'\n\
             private_key_path = '/etc/teslatlas/tls/key.pem'\n\
             public_url = 'https://hub.example.test'",
        )
        .expect("parse configuration");
        config.validate().expect("safe remote TLS configuration");

        let invalid: HubConfig = toml::from_str(
            "data_dir = '/var/lib/teslatlas'\nbind = '0.0.0.0:8443'\n\
             [tls]\ncertificate_path = '/etc/teslatlas/tls/cert.pem'\n\
             private_key_path = '/etc/teslatlas/tls/key.pem'\n\
             public_url = 'http://hub.example.test?token=secret'",
        )
        .expect("parse unsafe configuration");
        assert!(matches!(
            invalid.validate(),
            Err(ConfigError::InvalidTlsPublicUrl)
        ));
    }

    #[test]
    fn teslamate_import_requires_a_complete_safe_source_configuration() {
        let incomplete: HubConfig = toml::from_str(
            "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
             [teslamate]\nsource_url = 'postgresql://reader@127.0.0.1/teslamate'",
        )
        .expect("parse");
        assert!(matches!(
            incomplete.validate(),
            Err(ConfigError::TeslaMatePartialConfiguration)
        ));

        let complete: HubConfig = toml::from_str(
            "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
             [teslamate]\nsource_url = 'postgresql://reader@127.0.0.1/teslamate'\n\
             source_key = 'garage-teslamate'",
        )
        .expect("parse");
        assert!(complete.validate().is_ok());
        assert_eq!(
            complete.teslamate.import_config().unwrap().source_key,
            "garage-teslamate"
        );

        let unsafe_stage: HubConfig = toml::from_str(
            "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
             [teslamate]\nsource_url = 'postgresql://reader@127.0.0.1/teslamate'\n\
             source_key = 'garage-teslamate'\nmaximum_stage_bytes = 1",
        )
        .expect("parse");
        assert!(matches!(
            unsafe_stage.validate(),
            Err(ConfigError::InvalidTeslaMateLimits)
        ));
    }
}
