// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fmt, fs,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

use rustix::{
    fs::{FileType, Mode, OFlags, fcntl_getfl, fcntl_setfl, fstat, open},
    process::getuid,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::{
    owner_api::{OwnerApiBase, OwnerApiOptions},
    protocol::Sha256Digest,
    teslamate::ReadOnlySource,
    teslamate_reader::TeslaMateReadLimits,
};

const MAX_CONFIG_BYTES: usize = 1024 * 1024;
pub(crate) const FLEET_TELEMETRY_INGRESS_PORT: u16 = 8080;

/// The patched Fleet receiver forwards plaintext telemetry to this fixed,
/// private endpoint. It is separate from the configured public Hub listener
/// whenever TLS is enabled.
pub(crate) fn fleet_telemetry_ingress_bind() -> SocketAddr {
    SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        FLEET_TELEMETRY_INGRESS_PORT,
    )
}

fn fleet_telemetry_listener_collides(bind: SocketAddr) -> bool {
    if bind.port() != FLEET_TELEMETRY_INGRESS_PORT {
        return false;
    }
    match bind.ip() {
        IpAddr::V4(address) => address.is_unspecified() || address == Ipv4Addr::LOCALHOST,
        // An IPv6 wildcard may also claim the IPv4 loopback port on platforms
        // with dual-stack sockets, so reject it conservatively.
        IpAddr::V6(address) => address.is_unspecified(),
    }
}

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
    #[serde(default)]
    pub http: HttpConfig,
    #[serde(default)]
    pub pairing: PairingConfig,
}

/// How long a paired device may keep renewing its bearer. Each claim writes
/// this span, counted from the claim, as the device's absolute maximum: the
/// bearer slides 30 days on use but never past it, and the device must pair
/// again afterwards. A change applies to devices paired after the Hub restarts;
/// existing devices keep the maximum they were given.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PairingConfig {
    #[serde(default = "default_device_max_lifetime_days")]
    pub device_max_lifetime_days: u32,
}

pub const MIN_DEVICE_MAX_LIFETIME_DAYS: u32 = 1;
pub const MAX_DEVICE_MAX_LIFETIME_DAYS: u32 = 3650;
const MILLISECONDS_PER_DAY: i64 = 24 * 60 * 60 * 1_000;

const fn default_device_max_lifetime_days() -> u32 {
    180
}

impl Default for PairingConfig {
    fn default() -> Self {
        Self {
            device_max_lifetime_days: default_device_max_lifetime_days(),
        }
    }
}

impl PairingConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(MIN_DEVICE_MAX_LIFETIME_DAYS..=MAX_DEVICE_MAX_LIFETIME_DAYS)
            .contains(&self.device_max_lifetime_days)
        {
            return Err(ConfigError::Invalid(
                "pairing.device_max_lifetime_days must be between 1 and 3650",
            ));
        }
        Ok(())
    }

    /// The absolute maximum lifetime in milliseconds. The configured value is
    /// at most 3650 days; an unvalidated larger one saturates instead of
    /// overflowing.
    pub fn device_max_lifetime_ms(&self) -> i64 {
        i64::from(self.device_max_lifetime_days).saturating_mul(MILLISECONDS_PER_DAY)
    }
}

/// Browser access is disabled until exact serialized origins are configured.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

impl HttpConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.allowed_origins.len() > 32 {
            return Err(ConfigError::Invalid(
                "http.allowed_origins is limited to 32 origins",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for origin in &self.allowed_origins {
            let parsed = Url::parse(origin).map_err(|_| {
                ConfigError::Invalid("http.allowed_origins must contain canonical HTTP(S) origins")
            })?;
            if origin.len() > 2048
                || !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
                || parsed.path() != "/"
                || parsed.origin().ascii_serialization() != *origin
                || !seen.insert(origin)
            {
                return Err(ConfigError::Invalid(
                    "http.allowed_origins must contain unique canonical HTTP(S) origins",
                ));
            }
        }
        Ok(())
    }
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
    #[serde(default = "default_terrain_max_cache_bytes")]
    pub max_cache_bytes: u64,
    #[serde(default = "default_terrain_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_terrain_read_timeout_seconds")]
    pub read_timeout_seconds: u64,
}

const fn default_terrain_enabled() -> bool {
    false
}

fn default_terrain_min_free_bytes() -> u64 {
    128 * 1024 * 1024
}
fn default_terrain_max_cache_bytes() -> u64 {
    512 * 1024 * 1024
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
            max_cache_bytes: default_terrain_max_cache_bytes(),
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
            || self.max_cache_bytes < crate::terrain::SRTM1_BYTES.saturating_add(16)
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
    #[serde(default)]
    pub provider: CollectorProvider,
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
    /// Driving read interval while the push stream is healthy (C3 R16). The
    /// stream delivers positions on its own, so reads only refresh the groups
    /// it lacks.
    #[serde(default = "default_driving_stream_poll_seconds")]
    pub driving_stream_poll_seconds: u64,
    /// Fleet polling cadence and local monthly cap (C3 R17). Every key is
    /// optional; the table is read only when `provider = "fleet"` collects by
    /// polling, but it is validated in every mode so a value kept for a later
    /// mode change is already sound.
    #[serde(default)]
    pub fleet_polling: FleetPollingConfig,
    /// Optional websocket endpoint override. Production overrides must use
    /// wss; ws is accepted only for loopback test endpoints.
    #[serde(default)]
    pub stream_endpoint_override: Option<String>,
    /// Region for provider tokens; absent means derive from the API host.
    #[serde(default)]
    pub stream_region: Option<crate::tesla_stream::StreamRegion>,
    /// Optional loopback Tesla vehicle-command proxy used by Fleet commands.
    /// Fleet wake remains a direct Fleet API call and does not use this proxy.
    #[serde(default)]
    pub fleet_command_proxy_url: Option<String>,
    /// Optional PEM trust root for the loopback Fleet command proxy.
    #[serde(default)]
    pub fleet_command_proxy_root_certificate_path: Option<PathBuf>,
    /// Native Fleet Telemetry receiver configuration. When present, Fleet
    /// collection is push-only and never falls back to paid vehicle-data
    /// polling.
    #[serde(default)]
    pub fleet_telemetry: Option<FleetTelemetryConfig>,
    #[serde(default = "default_stream_health_timeout_seconds")]
    pub stream_health_timeout_seconds: u64,
    /// Enables explicit legacy owner-auth refresh calls. No secret is read
    /// from configuration, and this mode is never used for Fleet TOKEN auth.
    #[serde(default)]
    pub legacy_auth: LegacyAuthConfig,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CollectorProvider {
    #[default]
    Legacy,
    Fleet,
}

impl fmt::Debug for CollectorConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CollectorConfig")
            .field("provider", &self.provider)
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
                "driving_stream_poll_seconds",
                &self.driving_stream_poll_seconds,
            )
            .field("fleet_polling", &self.fleet_polling)
            .field(
                "stream_endpoint_override",
                &self.stream_endpoint_override.as_ref().map(|_| "[redacted]"),
            )
            .field("stream_region", &self.stream_region)
            .field(
                "fleet_command_proxy_url",
                &self.fleet_command_proxy_url.as_ref().map(|_| "[loopback]"),
            )
            .field(
                "fleet_command_proxy_root_certificate_path",
                &self
                    .fleet_command_proxy_root_certificate_path
                    .as_ref()
                    .map(|_| "[configured]"),
            )
            .field(
                "fleet_telemetry",
                &self.fleet_telemetry.as_ref().map(|_| "[configured]"),
            )
            .field(
                "stream_health_timeout_seconds",
                &self.stream_health_timeout_seconds,
            )
            .field("legacy_auth", &self.legacy_auth)
            .finish()
    }
}

/// Tesla's global Owner API origin: the default `owner_api_base_url` and one
/// of the two origins that make a collector Legacy rather than Provider
/// (C3 R1).
const TESLA_OWNER_API_GLOBAL_URL: &str = "https://owner-api.teslamotors.com/";

/// Tesla's China Owner API origin: selected for China tokens when the global
/// default is configured, and the second origin that keeps a collector Legacy
/// (C3 R1).
const TESLA_OWNER_API_CHINA_URL: &str = "https://owner-api.vn.cloud.tesla.cn/";

/// `request_timeout_seconds` default (C3 R21, provisional until gate M-T1).
/// Tesla's servers answer 408 or 503 themselves when a car does not respond,
/// so the client timeout only guards network stalls; 15 s delays a vehicle's
/// next read by at most one and a half fallback intervals (R16).
const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 15;

/// Smallest `request_timeout_seconds` (C3 R21): zero would give a request no
/// time at all.
const MIN_REQUEST_TIMEOUT_SECONDS: u64 = 1;

/// Largest `request_timeout_seconds` (C3 R21): twice the longest single
/// operation Tesla publishes (a wake takes 10 to 60 s), so no legitimate
/// response is cut off, while a stalled connection cannot hold a tick's
/// request slot for minutes.
const MAX_REQUEST_TIMEOUT_SECONDS: u64 = 120;

/// `interval_seconds` default, the Owner-wire inventory list interval (C3 R16,
/// R20): a car added to or removed from the account is seen within five
/// minutes. The list only runs when no state check is already due.
const DEFAULT_INVENTORY_INTERVAL_SECONDS: u64 = 300;

/// `max_backoff_seconds` default (C3 R44): half of `T_gap`, so a vehicle in a
/// session is retried at least twice in every `T_gap` window and a recovering
/// API is read before the lifecycle would end the drive for lack of evidence;
/// at the cap, attempts are at least 150 s apart, so a failing vehicle makes
/// at most 24 requests an hour.
const DEFAULT_MAX_BACKOFF_SECONDS: u64 = 300;

/// `driving_poll_milliseconds` default, the Owner-wire Driving read interval
/// when no stream is healthy (C3 R16, R18): spacing between positions is
/// speed times interval, 139 m at 50 km/h and 333 m at 120 km/h, which keeps
/// urban spacing near one street block, enough to place every turn. The
/// fastest row (six requests a minute) uses one tenth of Tesla's published
/// realtime limit of 60 a minute per device, leaving room for the owner's
/// phone app and other clients that share it.
const DEFAULT_DRIVING_POLL_MILLISECONDS: u64 = 10_000;

/// Smallest `driving_poll_milliseconds` (C3 R21): Tesla's published realtime
/// data limit is 60 requests a minute per device and account, so no read may
/// be faster than one a second.
const MIN_DRIVING_POLL_MILLISECONDS: u64 = 1_000;

/// `driving_stream_poll_seconds` default, the Owner-wire Driving read interval
/// while the stream is healthy (C3 R16, R18): the stream delivers positions at
/// its own rate, so reads only refresh the groups it lacks (temperatures,
/// charge and vehicle state), and 60 s gives a five-minute drive five
/// temperature samples.
const DEFAULT_DRIVING_STREAM_POLL_SECONDS: u64 = 60;

/// `charging_poll_seconds` default, the Owner-wire Charging read interval
/// (C3 R16, R19): energy per interval is power times interval, 0.092 kWh at
/// 11 kW AC and 1.25 kWh at 150 kW DC, under 2 % of an illustrative 75 kWh
/// pack, so a DC curve is resolved in steps below 2 % of charge. Requests are
/// free on the Owner wire, so AC is over-sampled by design.
const DEFAULT_CHARGING_POLL_SECONDS: u64 = 30;

/// `online_poll_seconds` default, the Owner-wire Online (idle window) read
/// interval (C3 R20): a drive or charge that starts while the car is idle and
/// no stream is healthy is seen within a minute (about 800 m at 50 km/h at
/// worst), and the idle window is short, so it costs about 15 reads per
/// parking.
const DEFAULT_ONLINE_POLL_SECONDS: u64 = 60;

/// `sleeping_poll_seconds` and `suspended_poll_seconds` default, the state
/// check interval for Asleep, Offline, Unknown and Suspended cars (C3 R16,
/// R20): in REST-only operation the delay before a wake is seen equals this
/// interval, and it is also the extra awake time a wake read can cause, so it
/// is kept short; one list request covers every car, so it does not grow with
/// the fleet.
const DEFAULT_STATE_CHECK_SECONDS: u64 = 30;

/// `updating_poll_seconds` default (C3 R20): the unofficial Owner API
/// documentation's example `software_update.expected_duration_sec` is
/// 2,700 s, and 120 s bounds the install's recorded end to within 4.4 % of
/// that duration.
const DEFAULT_UPDATING_POLL_SECONDS: u64 = 120;

/// `idle_suspend_after_seconds` default (C3 R21, R61). The key stays readable
/// so old configurations load, but the per-car `suspend_after_idle_min`
/// governs; the default equals that setting's live default of 15 minutes
/// (C3 R58), so only a different value is reported as ignored.
const DEFAULT_IDLE_SUSPEND_AFTER_SECONDS: u64 = 900;

/// `stream_health_timeout_seconds` default and lower bound (C3 R21; C4 R30,
/// R56 d): the session data-silence limit `H`. Below 60 s the stream's
/// dead-socket rule would not classify a lost socket first, and 60 s detects
/// a lost subscription soonest.
const MIN_STREAM_HEALTH_TIMEOUT_SECONDS: u64 = 60;

/// Upper bound of `stream_health_timeout_seconds` (C3 R21; C4 R30): half of
/// `T_gap` (600 s).
const MAX_STREAM_HEALTH_TIMEOUT_SECONDS: u64 = 300;

/// Fleet Driving read interval default (C3 R17, R18; owner decision of
/// 2026-10-08): one hour of driving is 120 reads, 24 cents, and 30 s gives
/// 417 m between positions at 50 km/h.
const DEFAULT_FLEET_DRIVING_POLL_SECONDS: u64 = 30;

/// Fleet Charging read interval default (C3 R17, R19; owner decision of
/// 2026-10-08): 0.37 kWh per interval at 11 kW AC and 5.0 kWh (6.7 %) at
/// 150 kW DC, which still shows the taper of a 30-minute DC session in 15
/// samples; two hours of charging cost 60 reads, 12 cents.
const DEFAULT_FLEET_CHARGING_POLL_SECONDS: u64 = 120;

/// Fleet Updating read interval default (C3 R17): bounds an install's
/// recorded end to five minutes, while most requests during an install return
/// 408, which Tesla bills (status below 500).
const DEFAULT_FLEET_UPDATING_POLL_SECONDS: u64 = 300;

/// Fleet Online (idle window) read interval default (C3 R17): an idle car
/// changes as slowly as an AC charge and the window is short (C3 R31), so it
/// matches the charging rate.
const DEFAULT_FLEET_ONLINE_POLL_SECONDS: u64 = 120;

/// Fleet state check interval default when state checks are billed (C3 R17):
/// billed state checks are the only way REST-only polling sees a wake, and at
/// 600 s they cost at most 4,464 requests per 31-day month ($8.93 at the data
/// rate), under Tesla's $10 monthly discount on their own, at the price of up
/// to ten minutes before a wake is seen.
const FLEET_STATE_CHECK_BILLED_SECONDS: u64 = 600;

/// Fleet state check interval default when state checks are not billed
/// (C3 R17): they cost nothing, so the Owner-wire interval applies for the
/// same reason (C3 R20).
const FLEET_STATE_CHECK_FREE_SECONDS: u64 = DEFAULT_STATE_CHECK_SECONDS;

/// Fleet inventory list interval default when lists are billed (C3 R17): the
/// list runs only when no state check is due, so it adds requests only while
/// every car is being read, and an hourly list still shows a car added to or
/// removed from the account within the hour.
const FLEET_INVENTORY_BILLED_SECONDS: u64 = 3_600;

/// Fleet inventory list interval default when lists are not billed
/// (C3 R17): the Owner-wire inventory interval, for the same reason (C3 R20).
const FLEET_INVENTORY_FREE_SECONDS: u64 = DEFAULT_INVENTORY_INTERVAL_SECONDS;

/// `monthly_cap_cents` default (C3 R27, owner item O3): it bounds the Hub's
/// own gross spend at Tesla's $10 monthly discount, so a configuration written
/// without an explicit figure spends no more than the discount covers when no
/// other application of the account uses it. With the defaults it stops
/// collection around day 14 of a month, which the consent screen states.
const DEFAULT_FLEET_MONTHLY_CAP_CENTS: u32 = 1_000;

const fn default_owner_api_timeout_seconds() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_SECONDS
}

fn default_owner_api_base_url() -> Option<String> {
    Some(TESLA_OWNER_API_GLOBAL_URL.to_owned())
}

const fn default_collector_interval_seconds() -> u64 {
    DEFAULT_INVENTORY_INTERVAL_SECONDS
}

const fn default_collector_max_backoff_seconds() -> u64 {
    DEFAULT_MAX_BACKOFF_SECONDS
}

const fn default_driving_poll_milliseconds() -> u64 {
    DEFAULT_DRIVING_POLL_MILLISECONDS
}

const fn default_driving_stream_poll_seconds() -> u64 {
    DEFAULT_DRIVING_STREAM_POLL_SECONDS
}

const fn default_charging_poll_seconds() -> u64 {
    DEFAULT_CHARGING_POLL_SECONDS
}

const fn default_online_poll_seconds() -> u64 {
    DEFAULT_ONLINE_POLL_SECONDS
}

const fn default_sleeping_poll_seconds() -> u64 {
    DEFAULT_STATE_CHECK_SECONDS
}

/// The silence tolerance `T_gap` of the lifecycle fold. It equals
/// `lifecycle::DEFAULT_OFFLINE_DRIVE_TIMEOUT` (C1 section 5) and the energy
/// fold's `K_INT_MAX_MS` (C2 section 7), so that a session the lifecycle keeps
/// continuous is also one whose intervals the energy integral bridges; a test
/// pins the three together.
const fn default_offline_drive_timeout_seconds() -> u64 {
    10 * 60
}

const fn default_idle_suspend_after_seconds() -> u64 {
    DEFAULT_IDLE_SUSPEND_AFTER_SECONDS
}

const fn default_suspended_poll_seconds() -> u64 {
    DEFAULT_STATE_CHECK_SECONDS
}

const fn default_updating_poll_seconds() -> u64 {
    DEFAULT_UPDATING_POLL_SECONDS
}

const fn default_stream_health_timeout_seconds() -> u64 {
    MIN_STREAM_HEALTH_TIMEOUT_SECONDS
}

const fn default_fleet_driving_poll_seconds() -> u64 {
    DEFAULT_FLEET_DRIVING_POLL_SECONDS
}

const fn default_fleet_charging_poll_seconds() -> u64 {
    DEFAULT_FLEET_CHARGING_POLL_SECONDS
}

const fn default_fleet_updating_poll_seconds() -> u64 {
    DEFAULT_FLEET_UPDATING_POLL_SECONDS
}

const fn default_fleet_online_poll_seconds() -> u64 {
    DEFAULT_FLEET_ONLINE_POLL_SECONDS
}

const fn default_fleet_monthly_cap_cents() -> u32 {
    DEFAULT_FLEET_MONTHLY_CAP_CENTS
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            provider: CollectorProvider::Legacy,
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
            driving_stream_poll_seconds: default_driving_stream_poll_seconds(),
            fleet_polling: FleetPollingConfig::default(),
            stream_endpoint_override: None,
            stream_region: None,
            fleet_command_proxy_url: None,
            fleet_command_proxy_root_certificate_path: None,
            fleet_telemetry: None,
            stream_health_timeout_seconds: default_stream_health_timeout_seconds(),
            legacy_auth: LegacyAuthConfig::default(),
        }
    }
}

/// The `[collector.fleet_polling]` table of consented Fleet polling (C3 R17).
/// Every key is optional. `state_poll_seconds` and `inventory_poll_seconds`
/// default by pricing: when Tesla bills state checks and lists they are
/// slow (600 s and 3,600 s), otherwise they match the Owner wire (30 s and
/// 300 s), so an absent key stays `None` here and `FleetPolicy::resolve`
/// decides.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FleetPollingConfig {
    #[serde(default = "default_fleet_driving_poll_seconds")]
    pub driving_poll_seconds: u64,
    #[serde(default = "default_fleet_charging_poll_seconds")]
    pub charging_poll_seconds: u64,
    #[serde(default = "default_fleet_updating_poll_seconds")]
    pub updating_poll_seconds: u64,
    #[serde(default = "default_fleet_online_poll_seconds")]
    pub online_poll_seconds: u64,
    /// Interval of the state check (a vehicle list) for Suspended, Asleep,
    /// Offline and Unknown cars; absent means the pricing default.
    #[serde(default)]
    pub state_poll_seconds: Option<u64>,
    /// Interval of the inventory list when no state check is due; absent means
    /// the pricing default.
    #[serde(default)]
    pub inventory_poll_seconds: Option<u64>,
    /// Local monthly cap on the gross cost of scheduled polling, in US cents
    /// (C3 R27). Zero stops all billable requests. The type is unsigned so a
    /// negative figure is a parse error, and 32 bits keep the cap's cost-unit
    /// arithmetic (cents times 1,500) far from overflow.
    #[serde(default = "default_fleet_monthly_cap_cents")]
    pub monthly_cap_cents: u32,
}

impl Default for FleetPollingConfig {
    fn default() -> Self {
        Self {
            driving_poll_seconds: default_fleet_driving_poll_seconds(),
            charging_poll_seconds: default_fleet_charging_poll_seconds(),
            updating_poll_seconds: default_fleet_updating_poll_seconds(),
            online_poll_seconds: default_fleet_online_poll_seconds(),
            state_poll_seconds: None,
            inventory_poll_seconds: None,
            monthly_cap_cents: default_fleet_monthly_cap_cents(),
        }
    }
}

impl FleetPollingConfig {
    /// Validate the table (C3 R21): every interval is greater than zero. The
    /// Fleet Driving interval is whole seconds, so it is at least one second
    /// (Tesla's realtime limit of 60 requests a minute). Checks run in the order
    /// of B2 clarification 3: driving, charging, updating, online, then a present
    /// state or inventory key.
    fn cadence(&self) -> Result<FleetCadence, CadenceViolation> {
        let positive = |key: &'static str, value: u64| -> Result<Duration, CadenceViolation> {
            if value == 0 {
                Err(CadenceViolation {
                    key,
                    value,
                    rule: CadenceRule::GreaterThanZero,
                })
            } else {
                Ok(Duration::from_secs(value))
            }
        };
        let driving = positive(
            "collector.fleet_polling.driving_poll_seconds",
            self.driving_poll_seconds,
        )?;
        let charging = positive(
            "collector.fleet_polling.charging_poll_seconds",
            self.charging_poll_seconds,
        )?;
        let updating = positive(
            "collector.fleet_polling.updating_poll_seconds",
            self.updating_poll_seconds,
        )?;
        let online = positive(
            "collector.fleet_polling.online_poll_seconds",
            self.online_poll_seconds,
        )?;
        let state_check = self
            .state_poll_seconds
            .map(|value| positive("collector.fleet_polling.state_poll_seconds", value))
            .transpose()?;
        let inventory = self
            .inventory_poll_seconds
            .map(|value| positive("collector.fleet_polling.inventory_poll_seconds", value))
            .transpose()?;
        Ok(FleetCadence {
            driving,
            charging,
            updating,
            online,
            state_check,
            inventory,
            monthly_cap_cents: self.monthly_cap_cents,
        })
    }
}

/// Validated Fleet polling settings (C3 R17). `state_check` and `inventory`
/// are `None` when the owner left them to the pricing default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetCadence {
    pub driving: Duration,
    pub charging: Duration,
    pub updating: Duration,
    pub online: Duration,
    pub state_check: Option<Duration>,
    pub inventory: Option<Duration>,
    pub monthly_cap_cents: u32,
}

impl FleetCadence {
    /// The settings of an absent `[collector.fleet_polling]` table.
    pub const DEFAULT: Self = Self {
        driving: Duration::from_secs(DEFAULT_FLEET_DRIVING_POLL_SECONDS),
        charging: Duration::from_secs(DEFAULT_FLEET_CHARGING_POLL_SECONDS),
        updating: Duration::from_secs(DEFAULT_FLEET_UPDATING_POLL_SECONDS),
        online: Duration::from_secs(DEFAULT_FLEET_ONLINE_POLL_SECONDS),
        state_check: None,
        inventory: None,
        monthly_cap_cents: DEFAULT_FLEET_MONTHLY_CAP_CENTS,
    };
}

/// Fleet request prices in requests per US dollar (C3 R24); `state: None`
/// means state checks are not billed. Part of `FleetPolicy` so tests can set
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetPricing {
    pub data: u32,
    pub state: Option<u32>,
    pub wake: u32,
    pub command: u32,
    pub signal: u32,
}

impl FleetPricing {
    /// True when Tesla bills a list or record request (C3 R17: "billed" is
    /// read from the pricing).
    pub const fn state_billed(&self) -> bool {
        self.state.is_some()
    }
}

/// Tesla's published Fleet API rates of 2026-10-11 (Tesla developer home
/// page, usage-based pricing: streaming signals 150,000, commands 1,000, data
/// 500 and wakes 50 requests per dollar; C3 R24, R26). `state` is
/// `Some(500)`, the data rate, until gate M-C1 records the "Pricing Category"
/// of Tesla's list and vehicle endpoint pages (their text rendering of
/// 2026-10-11 does not show it): counting an unverified category as billed
/// errs towards over-estimating, the safe side for a cap. Changing a rate is a
/// dated decision.
pub const FLEET_PRICING: FleetPricing = FleetPricing {
    data: 500,
    state: Some(500),
    wake: 50,
    command: 1_000,
    signal: 150_000,
};

/// Fleet polling intervals after the pricing default is applied (C3 R17).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetIntervals {
    pub driving: Duration,
    pub charging: Duration,
    pub updating: Duration,
    pub online: Duration,
    /// Interval of the state check for Suspended, Asleep, Offline and Unknown
    /// cars.
    pub state_check: Duration,
    /// Interval of the inventory list when no state check is due.
    pub inventory: Duration,
}

/// Everything the scheduler needs for Fleet polling (C3 R3a): intervals, the
/// local monthly cap and the pricing in force.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetPolicy {
    pub intervals: FleetIntervals,
    pub monthly_cap_cents: u32,
    pub pricing: FleetPricing,
}

impl FleetPolicy {
    /// Apply `pricing` to the validated settings: an owner-set state check or
    /// inventory interval always wins; otherwise they are slow when Tesla
    /// bills them and match the Owner wire when it does not (C3 R17).
    pub fn resolve(cadence: &FleetCadence, pricing: FleetPricing) -> Self {
        let billed = pricing.state_billed();
        let (state_default, inventory_default) = if billed {
            (
                FLEET_STATE_CHECK_BILLED_SECONDS,
                FLEET_INVENTORY_BILLED_SECONDS,
            )
        } else {
            (FLEET_STATE_CHECK_FREE_SECONDS, FLEET_INVENTORY_FREE_SECONDS)
        };
        Self {
            intervals: FleetIntervals {
                driving: cadence.driving,
                charging: cadence.charging,
                updating: cadence.updating,
                online: cadence.online,
                state_check: cadence
                    .state_check
                    .unwrap_or(Duration::from_secs(state_default)),
                inventory: cadence
                    .inventory
                    .unwrap_or(Duration::from_secs(inventory_default)),
            },
            monthly_cap_cents: cadence.monthly_cap_cents,
            pricing,
        }
    }
}

/// How the collector reaches Tesla (C3 R1). Legacy and Provider use the
/// Owner-wire cadence table, Fleet polling the Fleet table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectorMode {
    /// The Owner API on Tesla's own host, with the legacy stream.
    Legacy,
    /// An Owner-API-compatible wire on a configured base URL and stream URL.
    Provider,
    /// Consented direct Fleet API polling with the owner's own application.
    FleetPolling,
}

impl CollectorMode {
    /// The value stored in `collector_account_state.mode` (C3 R4).
    pub const fn code(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Provider => "provider",
            Self::FleetPolling => "fleet_polling",
        }
    }

    /// True for the two modes that share the Owner-wire cadence table.
    pub const fn is_owner_wire(self) -> bool {
        matches!(self, Self::Legacy | Self::Provider)
    }
}

/// The normalised origin of the configured Owner API base as text, empty when
/// none is configured or it does not parse. It is one input of the
/// configuration fingerprint (C3 R4, R7): a changed provider origin must reset
/// the collector's stored refusals.
pub(crate) fn owner_api_origin_text(config: &CollectorConfig) -> String {
    config
        .owner_api_base_url
        .as_deref()
        .and_then(normalised_origin)
        .map(|(scheme, host, port)| format!("{scheme}://{host}:{port}"))
        .unwrap_or_default()
}

/// Scheme, lowercased host without one trailing dot, and port with the
/// scheme's default made explicit (RFC 3986 sections 6.2.2 and 6.2.3); the
/// path, query and a trailing slash are ignored.
fn normalised_origin(raw: &str) -> Option<(String, String, u16)> {
    let url = Url::parse(raw).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host).to_owned();
    Some((
        url.scheme().to_ascii_lowercase(),
        host,
        url.port_or_known_default()?,
    ))
}

/// The collector's mode (C3 R1).
///
/// `provider = "fleet"` is Fleet polling. Otherwise the mode is Provider
/// exactly when the origin of `owner_api_base_url` differs from every Tesla
/// Owner API origin the Hub ships (the global and China defaults), else
/// Legacy, so a written-out default, a trailing slash, an upper-case host or
/// an explicit default port mean what the owner meant. The data host is what
/// makes a provider a provider: `[collector.legacy_auth] token_url` never
/// selects the mode, because a Tesla login's refresh token must not be sent to
/// a configured URL without the issuer pin (C5 R7). C5's mode input reads
/// this same function, so the two units cannot disagree.
///
/// An absent `owner_api_base_url` has no data host and is Legacy (nothing can
/// be requested without one, and validation asks for it). An unparsable one
/// has no origin that equals Tesla's and is Provider, the choice that applies
/// no Tesla-specific behaviour; validation rejects it before a collector
/// starts.
pub fn collector_mode(config: &CollectorConfig) -> CollectorMode {
    if config.provider == CollectorProvider::Fleet {
        return CollectorMode::FleetPolling;
    }
    let Some(base) = config.owner_api_base_url.as_deref() else {
        return CollectorMode::Legacy;
    };
    let Some(origin) = normalised_origin(base) else {
        return CollectorMode::Provider;
    };
    let tesla = [TESLA_OWNER_API_GLOBAL_URL, TESLA_OWNER_API_CHINA_URL]
        .into_iter()
        .filter_map(normalised_origin)
        .any(|tesla_origin| tesla_origin == origin);
    if tesla {
        CollectorMode::Legacy
    } else {
        CollectorMode::Provider
    }
}

/// A condition of the collector configuration that is reported once at start
/// (C3 R21). The configuration still loads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigNotice {
    /// `config_value_clamped`: a value outside its range was moved into it.
    ValueClamped {
        key: &'static str,
        configured: u64,
        effective: u64,
    },
    /// `config_key_ignored`: a readable key that no longer has any effect was
    /// set to a non-default value.
    KeyIgnored { key: &'static str },
}

impl ConfigNotice {
    /// The event code of the decision log.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ValueClamped { .. } => "config_value_clamped",
            Self::KeyIgnored { .. } => "config_key_ignored",
        }
    }
}

/// Public Tesla Fleet Telemetry endpoint and its local Hub ingress secret.
///
/// The hostname is sent to the vehicle. TLS termination happens in the
/// companion receiver; the Rust Hub uses the fixed private loopback ingress
/// alongside its configured public TLS listener when both are enabled.
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FleetTelemetryConfig {
    pub hostname: String,
    #[serde(default = "default_fleet_telemetry_port")]
    pub port: u16,
    pub ca_certificate_path: PathBuf,
    pub ingest_token_path: PathBuf,
}

impl fmt::Debug for FleetTelemetryConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FleetTelemetryConfig")
            .field("hostname", &self.hostname)
            .field("port", &self.port)
            .field("ca_certificate_path", &"[configured]")
            .field("ingest_token_path", &"[redacted]")
            .finish()
    }
}

const fn default_fleet_telemetry_port() -> u16 {
    443
}

impl FleetTelemetryConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        let hostname = self.hostname.as_str();
        let hostname_valid = !hostname.is_empty()
            && hostname.len() <= 253
            && hostname.is_ascii()
            && !hostname.starts_with('.')
            && !hostname.ends_with('.')
            && hostname.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            });
        if !hostname_valid || self.port == 0 {
            return Err(ConfigError::InvalidFleetTelemetry);
        }
        if !self.ca_certificate_path.is_absolute()
            || !self.ingest_token_path.is_absolute()
            || self.ca_certificate_path == self.ingest_token_path
        {
            return Err(ConfigError::InvalidFleetTelemetry);
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyAuthConfig {
    #[serde(default)]
    pub enabled: bool,
    /// The token endpoint of an Owner-API-compatible provider (C5 R7). Provider
    /// mode only: Tesla mode forbids it, so the issuer pin cannot be bypassed.
    /// It must be an `https` URL with a host and no userinfo, query or
    /// fragment; the path is the provider's own.
    #[serde(default)]
    pub token_url: Option<String>,
}

impl Default for LegacyAuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            token_url: None,
        }
    }
}

impl fmt::Debug for LegacyAuthConfig {
    /// The `token_url` path belongs to the provider and is never shown (C5
    /// R53); only whether the key is set appears.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LegacyAuthConfig")
            .field("enabled", &self.enabled)
            .field("token_url", &self.token_url.as_ref().map(|_| "[redacted]"))
            .finish()
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
    false
}

impl GeocoderConfig {
    pub(crate) fn endpoint_url(&self, test_mode: bool) -> Result<Url, ConfigError> {
        let raw = self
            .endpoint
            .as_deref()
            .ok_or(ConfigError::InvalidGeocoderEndpoint)?;
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

/// The first cadence rule a configuration breaks (C3 R21, B2 clarification 3):
/// the full TOML key, its value in the key's own unit (milliseconds for
/// `collector.driving_poll_milliseconds`, whole seconds for the others) and the
/// rule. It carries no path, credential or vehicle data, so it can be logged and
/// shown. Its text is `collector setting <key> = <value> <rule clause>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CadenceViolation {
    pub key: &'static str,
    pub value: u64,
    pub rule: CadenceRule,
}

/// The rule of a [`CadenceViolation`]. A relative rule keeps the key and value
/// it was compared with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CadenceRule {
    GreaterThanZero,
    MinimumMilliseconds(u64),
    SecondsRange {
        minimum: u64,
        maximum: u64,
    },
    BelowIntegrationBound {
        seconds: u64,
    },
    BelowOfflineTimeout {
        bound_key: &'static str,
        bound_seconds: u64,
    },
}

impl CadenceRule {
    /// The clause that follows `<key> = <value>` in the message.
    pub fn clause(&self) -> String {
        match self {
            Self::GreaterThanZero => "must be greater than zero".to_owned(),
            Self::MinimumMilliseconds(minimum) => {
                format!("must be at least {minimum} milliseconds")
            }
            Self::SecondsRange { minimum, maximum } => {
                format!("must be between {minimum} and {maximum} seconds inclusive")
            }
            Self::BelowIntegrationBound { seconds } => format!(
                "must be below {seconds} seconds, the longest charging interval the Hub integrates"
            ),
            Self::BelowOfflineTimeout {
                bound_key,
                bound_seconds,
            } => format!("must be below {bound_key} ({bound_seconds})"),
        }
    }
}

impl CadenceRule {
    /// A stable word for the rule, for machine-readable reports.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::GreaterThanZero => "greater_than_zero",
            Self::MinimumMilliseconds(_) => "minimum_milliseconds",
            Self::SecondsRange { .. } => "seconds_range",
            Self::BelowIntegrationBound { .. } => "below_integration_bound",
            Self::BelowOfflineTimeout { .. } => "below_offline_timeout",
        }
    }
}

/// The shape a status or doctor report gives a [`CadenceViolation`]:
/// `{key, value, rule, message}`. Secret-free by construction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CadenceViolationReport {
    pub key: &'static str,
    pub value: u64,
    pub rule: &'static str,
    pub message: String,
}

impl CadenceViolation {
    /// The code of the attention a cadence violation raises (B2 clarification 3).
    pub const CODE: &'static str = "collector_cadence_invalid";

    pub fn report(&self) -> CadenceViolationReport {
        CadenceViolationReport {
            key: self.key,
            value: self.value,
            rule: self.rule.code(),
            message: self.to_string(),
        }
    }
}

impl fmt::Display for CadenceViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "collector setting {} = {} {}",
            self.key,
            self.value,
            self.rule.clause()
        )
    }
}

impl std::error::Error for CadenceViolation {}

/// The validated collector cadence (C3 R16, R17, R21). Owner-wire intervals
/// are here; the Fleet table is in `fleet` and gets its pricing default from
/// `fleet_policy`. `idle_suspend_after_seconds` is not part of it: the key is
/// read but ignored, and the per-car `suspend_after_idle_min` governs
/// (C3 R61).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectorCadence {
    /// Owner-wire Driving read interval with no healthy stream.
    pub driving: Duration,
    /// Owner-wire Driving read interval while the stream is healthy.
    pub driving_stream: Duration,
    pub charging: Duration,
    pub online: Duration,
    /// State check interval for Asleep, Offline and Unknown cars.
    pub sleeping: Duration,
    pub offline_drive_timeout: Duration,
    /// State check interval for Suspended cars.
    pub suspended: Duration,
    pub updating: Duration,
    /// The Owner-wire inventory list interval (`interval_seconds`). Zero only
    /// when supervised collection is disabled, and then no scheduler is built.
    pub inventory: Duration,
    /// The stream's session data-silence limit, already clamped to its range
    /// (C3 R21).
    pub stream_health_timeout: Duration,
    pub maximum_backoff: Duration,
    pub fleet: FleetCadence,
}

impl CollectorCadence {
    /// The Fleet policy with Tesla's published pricing (C3 R3a).
    pub fn fleet_policy(&self) -> FleetPolicy {
        FleetPolicy::resolve(&self.fleet, FLEET_PRICING)
    }
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
                == OwnerApiBase::parse(TESLA_OWNER_API_GLOBAL_URL)
                    .map_err(|_| ConfigError::InvalidOwnerApiBase)?
        {
            base_url = OwnerApiBase::parse(TESLA_OWNER_API_CHINA_URL)
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

    /// The stream's session data-silence limit in seconds, moved into the range
    /// 60 to 300 (C3 R21). Clamping instead of rejecting keeps an old configuration
    /// loading (G2-10: an old configuration keeps working for one release).
    pub fn effective_stream_health_timeout_seconds(&self) -> u64 {
        self.stream_health_timeout_seconds.clamp(
            MIN_STREAM_HEALTH_TIMEOUT_SECONDS,
            MAX_STREAM_HEALTH_TIMEOUT_SECONDS,
        )
    }

    /// The longest back-off in seconds the collector uses (C3 R21; B2 round 3,
    /// CMP3-01, coordinator decision; B2 apply, R3C-04, changed). The previous
    /// release's default (900 s) and its configuration guide's example set
    /// `max_backoff_seconds` above the offline timeout that this release enforces, and
    /// a file that only lowers the timeout leaves this release's own default (300 s) at
    /// or above it, so for one release a back-off, explicit or default, at or above
    /// `offline_drive_timeout_seconds` is moved to the largest valid value, one second
    /// below the timeout, instead of being a cadence error that would leave Serve
    /// waiting without collecting after the upgrade (G2-10: an old configuration keeps
    /// working for one release). A timeout of one second leaves no valid value, so that
    /// case stays the cadence error. Every other newly enforced bound stays an error.
    pub fn effective_max_backoff_seconds(&self) -> u64 {
        let timeout = self.offline_drive_timeout_seconds;
        if timeout > 1 && self.max_backoff_seconds >= timeout {
            timeout - 1
        } else {
            self.max_backoff_seconds
        }
    }

    /// Validate every collector interval and return the cadence (C3 R21).
    ///
    /// Reasons per rule:
    /// - every interval is greater than zero, `driving_poll_milliseconds` is at
    ///   least 1,000 and the Fleet Driving interval at least one second,
    ///   because Tesla's published realtime limit is 60 requests a minute;
    /// - the Driving, Charging and Online read intervals of the active mode and
    ///   `max_backoff_seconds` are each below `offline_drive_timeout_seconds`
    ///   (an explicit back-off at or above it is moved below it, see
    ///   [`Self::effective_max_backoff_seconds`]), because an interval at or above
    ///   `T_gap` would make the lifecycle close every drive between two polls;
    /// - both charging intervals (the Owner-wire and the Fleet key) are below
    ///   the energy fold's `K_INT_MAX_MS` in every mode, whatever
    ///   `offline_drive_timeout_seconds` is: the energy integral treats a
    ///   longer interval as an unpowered gap (C2 R21), so such a cadence would
    ///   leave every ordinary interval unintegrated, the bound is C2's fixed
    ///   constant rather than `T_gap`, and a value kept for a later mode
    ///   change should already be valid;
    /// - `request_timeout_seconds` is between 1 and 120.
    ///
    /// `stream_health_timeout_seconds` is clamped, not rejected, and
    /// `interval_seconds` is not checked here because an explicit zero
    /// disables supervised collection (`supervised_interval`).
    ///
    /// The first failure, in a fixed order (B2 clarification 3), names its full
    /// key, value and rule. Loading a configuration does not run these rules:
    /// an old file with a newly invalid cadence still loads, and managed Serve
    /// reports the failure (`cadence_violation`) instead of refusing to start.
    pub fn cadence(&self) -> Result<CollectorCadence, ConfigError> {
        self.checked_cadence()
            .map_err(ConfigError::InvalidCollectorCadence)
    }

    /// The first violated cadence rule, if any, without building the cadence.
    /// The two zero-check groups, the driving minimum, the timeout range, the
    /// charging integration bound and the relative bounds are tested in that
    /// order, so one broken file always reports the same setting first.
    pub fn cadence_violation(&self) -> Option<CadenceViolation> {
        self.checked_cadence().err()
    }

    fn checked_cadence(&self) -> Result<CollectorCadence, CadenceViolation> {
        // Fleet zero intervals come first, in every mode.
        let fleet = self.fleet_polling.cadence()?;
        let positive = |key: &'static str, value: u64| -> Result<(), CadenceViolation> {
            if value == 0 {
                Err(CadenceViolation {
                    key,
                    value,
                    rule: CadenceRule::GreaterThanZero,
                })
            } else {
                Ok(())
            }
        };
        positive(
            "collector.driving_poll_milliseconds",
            self.driving_poll_milliseconds,
        )?;
        positive(
            "collector.driving_stream_poll_seconds",
            self.driving_stream_poll_seconds,
        )?;
        positive(
            "collector.charging_poll_seconds",
            self.charging_poll_seconds,
        )?;
        positive("collector.online_poll_seconds", self.online_poll_seconds)?;
        positive(
            "collector.sleeping_poll_seconds",
            self.sleeping_poll_seconds,
        )?;
        positive(
            "collector.offline_drive_timeout_seconds",
            self.offline_drive_timeout_seconds,
        )?;
        positive(
            "collector.suspended_poll_seconds",
            self.suspended_poll_seconds,
        )?;
        positive(
            "collector.updating_poll_seconds",
            self.updating_poll_seconds,
        )?;
        positive("collector.max_backoff_seconds", self.max_backoff_seconds)?;
        let cadence = CollectorCadence {
            driving: Duration::from_millis(self.driving_poll_milliseconds),
            driving_stream: Duration::from_secs(self.driving_stream_poll_seconds),
            charging: Duration::from_secs(self.charging_poll_seconds),
            online: Duration::from_secs(self.online_poll_seconds),
            sleeping: Duration::from_secs(self.sleeping_poll_seconds),
            offline_drive_timeout: Duration::from_secs(self.offline_drive_timeout_seconds),
            suspended: Duration::from_secs(self.suspended_poll_seconds),
            updating: Duration::from_secs(self.updating_poll_seconds),
            inventory: Duration::from_secs(self.interval_seconds),
            stream_health_timeout: Duration::from_secs(
                self.effective_stream_health_timeout_seconds(),
            ),
            maximum_backoff: Duration::from_secs(self.effective_max_backoff_seconds()),
            fleet,
        };
        if self.driving_poll_milliseconds < MIN_DRIVING_POLL_MILLISECONDS {
            return Err(CadenceViolation {
                key: "collector.driving_poll_milliseconds",
                value: self.driving_poll_milliseconds,
                rule: CadenceRule::MinimumMilliseconds(MIN_DRIVING_POLL_MILLISECONDS),
            });
        }
        if !(MIN_REQUEST_TIMEOUT_SECONDS..=MAX_REQUEST_TIMEOUT_SECONDS)
            .contains(&self.request_timeout_seconds)
        {
            return Err(CadenceViolation {
                key: "collector.request_timeout_seconds",
                value: self.request_timeout_seconds,
                rule: CadenceRule::SecondsRange {
                    minimum: MIN_REQUEST_TIMEOUT_SECONDS,
                    maximum: MAX_REQUEST_TIMEOUT_SECONDS,
                },
            });
        }
        // C2's constant, in milliseconds; both charging keys in every mode.
        let integration_bound_ms =
            u64::try_from(crate::lifecycle::K_INT_MAX_MS).unwrap_or(u64::MAX);
        let integration_bound = Duration::from_millis(integration_bound_ms);
        for (key, value, interval) in [
            (
                "collector.charging_poll_seconds",
                self.charging_poll_seconds,
                cadence.charging,
            ),
            (
                "collector.fleet_polling.charging_poll_seconds",
                self.fleet_polling.charging_poll_seconds,
                cadence.fleet.charging,
            ),
        ] {
            if interval >= integration_bound {
                return Err(CadenceViolation {
                    key,
                    value,
                    rule: CadenceRule::BelowIntegrationBound {
                        seconds: integration_bound_ms / 1_000,
                    },
                });
            }
        }
        // The active mode's session intervals, then the back-off, each below the
        // silence tolerance (C3 R21).
        let session_intervals: Vec<(&'static str, u64, Duration)> =
            if collector_mode(self).is_owner_wire() {
                vec![
                    (
                        "collector.driving_poll_milliseconds",
                        self.driving_poll_milliseconds,
                        cadence.driving,
                    ),
                    (
                        "collector.driving_stream_poll_seconds",
                        self.driving_stream_poll_seconds,
                        cadence.driving_stream,
                    ),
                    (
                        "collector.charging_poll_seconds",
                        self.charging_poll_seconds,
                        cadence.charging,
                    ),
                    (
                        "collector.online_poll_seconds",
                        self.online_poll_seconds,
                        cadence.online,
                    ),
                ]
            } else {
                vec![
                    (
                        "collector.fleet_polling.driving_poll_seconds",
                        self.fleet_polling.driving_poll_seconds,
                        cadence.fleet.driving,
                    ),
                    (
                        "collector.fleet_polling.charging_poll_seconds",
                        self.fleet_polling.charging_poll_seconds,
                        cadence.fleet.charging,
                    ),
                    (
                        "collector.fleet_polling.online_poll_seconds",
                        self.fleet_polling.online_poll_seconds,
                        cadence.fleet.online,
                    ),
                ]
            };
        for (key, value, interval) in session_intervals.into_iter().chain([(
            "collector.max_backoff_seconds",
            self.max_backoff_seconds,
            cadence.maximum_backoff,
        )]) {
            if interval >= cadence.offline_drive_timeout {
                return Err(CadenceViolation {
                    key,
                    value,
                    rule: CadenceRule::BelowOfflineTimeout {
                        bound_key: "collector.offline_drive_timeout_seconds",
                        bound_seconds: self.offline_drive_timeout_seconds,
                    },
                });
            }
        }
        Ok(cadence)
    }

    /// Conditions that are reported once at start and do not stop the
    /// collector (C3 R21): a stream health timeout outside its range was
    /// clamped, and a non-default `idle_suspend_after_seconds` is read but
    /// ignored.
    pub fn start_notices(&self) -> Vec<ConfigNotice> {
        let mut notices = Vec::new();
        let effective = self.effective_stream_health_timeout_seconds();
        if effective != self.stream_health_timeout_seconds {
            notices.push(ConfigNotice::ValueClamped {
                key: "stream_health_timeout_seconds",
                configured: self.stream_health_timeout_seconds,
                effective,
            });
        }
        let backoff = self.effective_max_backoff_seconds();
        if backoff != self.max_backoff_seconds {
            notices.push(ConfigNotice::ValueClamped {
                key: "max_backoff_seconds",
                configured: self.max_backoff_seconds,
                effective: backoff,
            });
        }
        if self.idle_suspend_after_seconds != DEFAULT_IDLE_SUSPEND_AFTER_SECONDS {
            notices.push(ConfigNotice::KeyIgnored {
                key: "idle_suspend_after_seconds",
            });
        }
        notices
    }

    /// Write [`Self::start_notices`] to the log, one event each. Called once
    /// per Hub start.
    pub fn log_start_notices(&self) {
        for notice in self.start_notices() {
            match notice {
                ConfigNotice::ValueClamped {
                    key,
                    configured,
                    effective,
                } => tracing::warn!(
                    event = notice.code(),
                    key,
                    configured,
                    effective,
                    "collector configuration value moved into its range"
                ),
                ConfigNotice::KeyIgnored { key } => tracing::warn!(
                    event = notice.code(),
                    key,
                    "collector configuration key is read but ignored; the per-car setting governs"
                ),
            }
        }
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
    /// Independent read-only current-state producer; absent means disabled.
    #[serde(default)]
    pub current: Option<TeslaMateCurrentConfig>,
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
            .field("current", &self.current.as_ref().map(|_| "[configured]"))
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

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeslaMateCurrentConfig {
    pub source_url: String,
    pub source_key: String,
    pub vehicle_id: uuid::Uuid,
    pub car_id: i64,
    pub password_file: PathBuf,
}
impl TeslaMateCurrentConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        let source = ReadOnlySource::parse(&self.source_url)
            .map_err(|_| ConfigError::Invalid("invalid TeslaMate current endpoint"))?;
        if source.user().is_none()
            || self.source_key.is_empty()
            || self.source_key.len() > 256
            || self.vehicle_id.is_nil()
            || self.car_id <= 0
            || self.car_id > i64::from(i16::MAX)
            || !self.password_file.is_absolute()
        {
            return Err(ConfigError::Invalid("invalid TeslaMate current binding"));
        }
        Ok(())
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
            current: None,
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
        let source = read_config_file(path.as_ref())?;
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
        // The retired Edge table is refused by name before typed parsing, in
        // table and dotted-key form. Malformed TOML keeps its ordinary parse
        // diagnostics.
        if let Ok(root) = source.parse::<toml::Table>()
            && root
                .get("collector")
                .and_then(toml::Value::as_table)
                .is_some_and(|collector| collector.contains_key("edge"))
        {
            return Err(ConfigError::EdgeDeliveryRetired);
        }
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
        self.http.validate()?;
        self.pairing.validate()?;
        if self.collector.request_timeout_seconds == 0 {
            return Err(ConfigError::InvalidOwnerApiTimeout);
        }
        if self.geocoder.enabled || self.geocoder.endpoint.is_some() {
            self.geocoder.endpoint_url(false)?;
        }
        self.geocoder.timeout()?;
        self.geocoder.validated_language()?;
        self.terrain.validate()?;
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
        if let Some(endpoint) = self.collector.fleet_command_proxy_url.as_deref() {
            crate::fleet_api::FleetCommandProxyBase::parse(endpoint)
                .map_err(|_| ConfigError::InvalidFleetCommandProxy)?;
        }
        if let Some(path) = self
            .collector
            .fleet_command_proxy_root_certificate_path
            .as_deref()
            && (!path.is_absolute() || self.collector.fleet_command_proxy_url.is_none())
        {
            return Err(ConfigError::InvalidFleetCommandProxy);
        }
        if let Some(telemetry) = &self.collector.fleet_telemetry {
            telemetry.validate()?;
            if self.collector.provider != CollectorProvider::Fleet
                || self.collector.fleet_command_proxy_url.is_none()
            {
                return Err(ConfigError::InvalidFleetTelemetry);
            }
            if let Some(_tls) = &self.tls {
                if fleet_telemetry_listener_collides(self.bind) {
                    return Err(ConfigError::FleetTelemetryListenerCollision);
                }
            } else if self.bind != fleet_telemetry_ingress_bind() {
                return Err(ConfigError::InvalidFleetTelemetry);
            }
        }
        let source_set = self.teslamate.source_url.is_some();
        let source_key_set = self.teslamate.source_key.is_some();
        if source_set != source_key_set {
            return Err(ConfigError::TeslaMatePartialConfiguration);
        }
        if source_set {
            self.teslamate.import_config()?;
        }
        if let Some(current) = &self.teslamate.current {
            current.validate()?;
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

fn read_config_file(path: &Path) -> Result<Vec<u8>, ConfigError> {
    read_config_file_after_open(path, || {})
}

fn read_config_file_after_open(
    path: &Path,
    after_open: impl FnOnce(),
) -> Result<Vec<u8>, ConfigError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| ConfigError::Read)?;
    let held = fstat(&descriptor).map_err(|_| ConfigError::Read)?;
    if !FileType::from_raw_mode(held.st_mode).is_file()
        || (held.st_uid != getuid().as_raw() && held.st_uid != 0)
        || (held.st_mode as u32 & 0o022) != 0
    {
        return Err(ConfigError::UnsafeFile);
    }
    let flags = fcntl_getfl(&descriptor).map_err(|_| ConfigError::Read)?;
    fcntl_setfl(&descriptor, flags & !OFlags::NONBLOCK).map_err(|_| ConfigError::Read)?;

    after_open();

    let file: fs::File = descriptor.into();
    let mut source = Vec::with_capacity(MAX_CONFIG_BYTES.min(8 * 1024));
    file.take(u64::try_from(MAX_CONFIG_BYTES + 1).expect("config cap fits u64"))
        .read_to_end(&mut source)
        .map_err(|_| ConfigError::Read)?;
    if source.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge);
    }

    let current = fs::symlink_metadata(path).map_err(|_| ConfigError::Read)?;
    if current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.uid() != held.st_uid
        || current.dev() != held.st_dev as u64
        || current.ino() != held.st_ino
        || (current.mode() & 0o022) != 0
    {
        return Err(ConfigError::FileIdentityChanged);
    }
    Ok(source)
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot safely read configuration")]
    Read,
    #[error("configuration exceeds the fixed size limit")]
    TooLarge,
    #[error("configuration file is unsafe")]
    UnsafeFile,
    #[error("configuration file identity changed while reading")]
    FileIdentityChanged,
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
    #[error("Fleet command proxy configuration is invalid or unsafe")]
    InvalidFleetCommandProxy,
    #[error("Fleet Telemetry configuration is invalid or unsafe")]
    InvalidFleetTelemetry,
    #[error(
        "Fleet Telemetry private ingress uses 127.0.0.1:8080; choose another TLS bind address or port"
    )]
    FleetTelemetryListenerCollision,
    #[error(
        "[collector.edge] is no longer supported: Edge delivery was retired. Remove the table; to stream telemetry, configure the self-hosted receiver in [collector.fleet_telemetry]."
    )]
    EdgeDeliveryRetired,
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
    #[error("{0}")]
    InvalidCollectorCadence(CadenceViolation),
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
#[path = "config/tests.rs"]
mod tests;
