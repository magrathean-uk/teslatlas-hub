use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::{
    owner_api::{OwnerApiBase, OwnerApiOptions},
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
    pub teslamate: TeslaMateConfig,
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

/// Explicit settings for a manual legacy owner-token compatibility read.
///
/// There is no default remote endpoint. Leaving `owner_api_base_url` unset
/// keeps collection unavailable while the ordinary Hub service remains fully
/// usable. The URL cannot contain credentials, query parameters, or a token.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectorConfig {
    #[serde(default)]
    pub owner_api_base_url: Option<String>,
    #[serde(default = "default_owner_api_timeout_seconds")]
    pub request_timeout_seconds: u64,
}

const fn default_owner_api_timeout_seconds() -> u64 {
    20
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            owner_api_base_url: None,
            request_timeout_seconds: default_owner_api_timeout_seconds(),
        }
    }
}

impl CollectorConfig {
    pub fn owner_api_options(&self) -> Result<OwnerApiOptions, ConfigError> {
        let base_url = self
            .owner_api_base_url
            .as_deref()
            .ok_or(ConfigError::OwnerApiBaseRequired)?;
        let base_url =
            OwnerApiBase::parse(base_url).map_err(|_| ConfigError::InvalidOwnerApiBase)?;
        let request_timeout = Duration::from_secs(self.request_timeout_seconds);
        if request_timeout.is_zero() {
            return Err(ConfigError::InvalidOwnerApiTimeout);
        }
        Ok(OwnerApiOptions::new(base_url, request_timeout))
    }
}

/// Opt-in TeslaMate import source. The endpoint excludes its password and the
/// source key is a durable owner label, not an endpoint alias.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeslaMateConfig {
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub source_key: Option<String>,
    #[serde(default = "default_teslamate_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_teslamate_page_size")]
    pub page_size: i32,
    #[serde(default = "default_teslamate_maximum_rows")]
    pub maximum_rows: usize,
    #[serde(default = "default_teslamate_maximum_stage_bytes")]
    pub maximum_stage_bytes: u64,
    #[serde(default = "default_teslamate_minimum_free_bytes")]
    pub minimum_free_bytes: u64,
}

const fn default_teslamate_connect_timeout_seconds() -> u64 {
    10
}

const fn default_teslamate_page_size() -> i32 {
    2_000
}

const fn default_teslamate_maximum_rows() -> usize {
    1_000_000
}

const fn default_teslamate_maximum_stage_bytes() -> u64 {
    4 * 1024 * 1024 * 1024
}

const fn default_teslamate_minimum_free_bytes() -> u64 {
    512 * 1024 * 1024
}

impl Default for TeslaMateConfig {
    fn default() -> Self {
        Self {
            source_url: None,
            source_key: None,
            connect_timeout_seconds: default_teslamate_connect_timeout_seconds(),
            page_size: default_teslamate_page_size(),
            maximum_rows: default_teslamate_maximum_rows(),
            maximum_stage_bytes: default_teslamate_maximum_stage_bytes(),
            minimum_free_bytes: default_teslamate_minimum_free_bytes(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TeslaMateImportConfig {
    pub source: ReadOnlySource,
    pub source_key: String,
    pub limits: TeslaMateReadLimits,
}

impl TeslaMateConfig {
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
        let limits = TeslaMateReadLimits {
            connect_timeout: Duration::from_secs(self.connect_timeout_seconds),
            page_size: self.page_size,
            maximum_rows: self.maximum_rows,
            maximum_stage_bytes: self.maximum_stage_bytes,
            minimum_free_bytes: self.minimum_free_bytes,
        };
        limits
            .validate()
            .map_err(|_| ConfigError::InvalidTeslaMateLimits)?;
        Ok(TeslaMateImportConfig {
            source,
            source_key: source_key.to_owned(),
            limits,
        })
    }
}

impl HubConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let config: Self = toml::from_str(&source).map_err(ConfigError::Parse)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.data_dir.as_os_str().is_empty() {
            return Err(ConfigError::Invalid("data_dir is required"));
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
        if let Some(base_url) = self.collector.owner_api_base_url.as_deref() {
            OwnerApiBase::parse(base_url).map_err(|_| ConfigError::InvalidOwnerApiBase)?;
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
    fn collector_stays_disabled_without_an_explicit_https_base() {
        let config: HubConfig =
            toml::from_str("data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'")
                .expect("valid config");
        assert!(matches!(
            config.collector.owner_api_options(),
            Err(ConfigError::OwnerApiBaseRequired)
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
             [teslamate]\nsource_url = 'postgresql://reader@db.internal/teslamate'",
        )
        .expect("parse");
        assert!(matches!(
            incomplete.validate(),
            Err(ConfigError::TeslaMatePartialConfiguration)
        ));

        let complete: HubConfig = toml::from_str(
            "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
             [teslamate]\nsource_url = 'postgresql://reader@db.internal/teslamate'\n\
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
             [teslamate]\nsource_url = 'postgresql://reader@db.internal/teslamate'\n\
             source_key = 'garage-teslamate'\nmaximum_stage_bytes = 1",
        )
        .expect("parse");
        assert!(matches!(
            unsafe_stage.validate(),
            Err(ConfigError::InvalidTeslaMateLimits)
        ));
    }
}
