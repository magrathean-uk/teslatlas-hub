use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use rcgen::{CertifiedKey, generate_simple_self_signed};
use thiserror::Error;
use uuid::Uuid;

use crate::config::HubConfig;

const DEFAULT_DATA_DIR: &str = "/var/lib/teslatlas";
const TLS_DIRECTORY: &str = "tls";

#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub config_path: PathBuf,
    pub lan_address: Option<IpAddr>,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupResult {
    pub public_url: String,
    pub bind: SocketAddr,
    pub certificate_path: PathBuf,
    pub private_key_path: PathBuf,
    pub created: bool,
}

pub fn configure(options: &SetupOptions) -> Result<SetupResult, SetupError> {
    if options.port == 0 {
        return Err(SetupError::InvalidPort);
    }
    if let Ok(existing) = HubConfig::load(&options.config_path)
        && let Some(tls) = existing.tls
    {
        require_regular_file(&tls.certificate_path)?;
        require_regular_file(&tls.private_key_path)?;
        return Ok(SetupResult {
            public_url: tls.public_url,
            bind: existing.bind,
            certificate_path: tls.certificate_path,
            private_key_path: tls.private_key_path,
            created: false,
        });
    }

    let lan_address = match options.lan_address {
        Some(address) => validate_lan_address(address)?,
        None => detect_lan_address()?,
    };
    let config_parent = options
        .config_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(SetupError::ConfigHasNoParent)?;
    fs::create_dir_all(config_parent).map_err(|source| SetupError::Io {
        operation: "create configuration directory",
        path: config_parent.to_path_buf(),
        source,
    })?;

    let tls_root = config_parent.join(TLS_DIRECTORY);
    ensure_directory_not_symlink(&tls_root)?;
    fs::create_dir_all(&tls_root).map_err(|source| SetupError::Io {
        operation: "create TLS directory",
        path: tls_root.clone(),
        source,
    })?;
    fs::set_permissions(&tls_root, fs::Permissions::from_mode(0o750)).map_err(|source| {
        SetupError::Io {
            operation: "protect TLS directory",
            path: tls_root.clone(),
            source,
        }
    })?;

    let generation = tls_root.join(format!("identity-{}", Uuid::new_v4()));
    fs::create_dir(&generation).map_err(|source| SetupError::Io {
        operation: "create TLS identity directory",
        path: generation.clone(),
        source,
    })?;
    fs::set_permissions(&generation, fs::Permissions::from_mode(0o750)).map_err(|source| {
        SetupError::Io {
            operation: "protect TLS identity directory",
            path: generation.clone(),
            source,
        }
    })?;

    let certificate_path = generation.join("certificate.pem");
    let private_key_path = generation.join("private-key.pem");
    let result = configure_new_identity(options, lan_address, &certificate_path, &private_key_path);
    if result.is_err() {
        let _ = fs::remove_dir_all(&generation);
    }
    result
}

fn configure_new_identity(
    options: &SetupOptions,
    lan_address: IpAddr,
    certificate_path: &Path,
    private_key_path: &Path,
) -> Result<SetupResult, SetupError> {
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec![lan_address.to_string(), "localhost".to_owned()])
            .map_err(SetupError::Certificate)?;
    write_new_file(certificate_path, cert.pem().as_bytes(), 0o644)?;
    write_new_file(
        private_key_path,
        signing_key.serialize_pem().as_bytes(),
        0o640,
    )?;

    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), options.port);
    let public_host = match lan_address {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    };
    let public_url = format!("https://{public_host}:{}", options.port);
    let config_source = updated_config(
        &options.config_path,
        bind,
        &public_url,
        certificate_path,
        private_key_path,
    )?;
    let parsed: HubConfig = toml::from_str(&config_source).map_err(SetupError::GeneratedConfig)?;
    parsed
        .validate()
        .map_err(|source| SetupError::GeneratedConfigInvalid(source.to_string()))?;
    write_config_atomically(&options.config_path, config_source.as_bytes())?;

    Ok(SetupResult {
        public_url,
        bind,
        certificate_path: certificate_path.to_path_buf(),
        private_key_path: private_key_path.to_path_buf(),
        created: true,
    })
}

fn updated_config(
    config_path: &Path,
    bind: SocketAddr,
    public_url: &str,
    certificate_path: &Path,
    private_key_path: &Path,
) -> Result<String, SetupError> {
    let mut root = if config_path.exists() {
        let source = fs::read_to_string(config_path).map_err(|source| SetupError::Io {
            operation: "read configuration",
            path: config_path.to_path_buf(),
            source,
        })?;
        source
            .parse::<toml::Table>()
            .map_err(SetupError::ExistingConfig)?
    } else {
        toml::Table::new()
    };
    root.entry("data_dir")
        .or_insert_with(|| toml::Value::String(DEFAULT_DATA_DIR.to_owned()));
    root.insert("bind".to_owned(), toml::Value::String(bind.to_string()));
    root.insert(
        "tls".to_owned(),
        toml::Value::Table(toml::Table::from_iter([
            (
                "certificate_path".to_owned(),
                toml::Value::String(certificate_path.display().to_string()),
            ),
            (
                "private_key_path".to_owned(),
                toml::Value::String(private_key_path.display().to_string()),
            ),
            (
                "public_url".to_owned(),
                toml::Value::String(public_url.to_owned()),
            ),
        ])),
    );
    toml::to_string_pretty(&root).map_err(SetupError::SerializeConfig)
}

fn write_new_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), SetupError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| SetupError::Io {
            operation: "create identity file",
            path: path.to_path_buf(),
            source,
        })?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|source| SetupError::Io {
            operation: "protect identity file",
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| SetupError::Io {
        operation: "write identity file",
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| SetupError::Io {
        operation: "sync identity file",
        path: path.to_path_buf(),
        source,
    })
}

fn write_config_atomically(path: &Path, bytes: &[u8]) -> Result<(), SetupError> {
    let parent = path.parent().ok_or(SetupError::ConfigHasNoParent)?;
    let temporary = parent.join(format!(".config-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        write_new_file(&temporary, bytes, 0o644)?;
        fs::rename(&temporary, path).map_err(|source| SetupError::Io {
            operation: "activate configuration",
            path: path.to_path_buf(),
            source,
        })?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| SetupError::Io {
                operation: "sync configuration directory",
                path: parent.to_path_buf(),
                source,
            })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn require_regular_file(path: &Path) -> Result<(), SetupError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| SetupError::Io {
        operation: "inspect existing identity",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SetupError::UnsafeIdentityPath(path.to_path_buf()));
    }
    Ok(())
}

fn ensure_directory_not_symlink(path: &Path) -> Result<(), SetupError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(SetupError::UnsafeIdentityPath(path.to_path_buf()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SetupError::Io {
            operation: "inspect TLS directory",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn detect_lan_address() -> Result<IpAddr, SetupError> {
    let socket =
        UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(SetupError::DetectLanAddress)?;
    socket
        .connect((Ipv4Addr::new(192, 0, 2, 1), 9))
        .map_err(SetupError::DetectLanAddress)?;
    validate_lan_address(
        socket
            .local_addr()
            .map_err(SetupError::DetectLanAddress)?
            .ip(),
    )
}

fn validate_lan_address(address: IpAddr) -> Result<IpAddr, SetupError> {
    if address.is_unspecified() || address.is_loopback() || address.is_multicast() {
        return Err(SetupError::InvalidLanAddress(address));
    }
    Ok(address)
}

#[derive(Debug, Error)]
pub enum SetupError {
    #[error("setup port must be greater than zero")]
    InvalidPort,
    #[error("configuration path must have a parent directory")]
    ConfigHasNoParent,
    #[error("cannot detect a reachable LAN address: {0}; pass --lan-address")]
    DetectLanAddress(io::Error),
    #[error("LAN address is not reachable by another device: {0}")]
    InvalidLanAddress(IpAddr),
    #[error("unsafe identity path: {0}")]
    UnsafeIdentityPath(PathBuf),
    #[error("cannot generate TLS certificate: {0}")]
    Certificate(rcgen::Error),
    #[error("existing configuration is invalid: {0}")]
    ExistingConfig(toml::de::Error),
    #[error("cannot serialize generated configuration: {0}")]
    SerializeConfig(toml::ser::Error),
    #[error("generated configuration is invalid: {0}")]
    GeneratedConfig(toml::de::Error),
    #[error("generated configuration failed validation: {0}")]
    GeneratedConfigInvalid(String),
    #[error("cannot {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_reuses_a_protected_lan_identity() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let config_path = temporary.path().join("etc/teslatlas/config.toml");
        fs::create_dir_all(config_path.parent().expect("config parent")).expect("config parent");
        fs::write(
            &config_path,
            "bind = \"127.0.0.1:4000\"\ndata_dir = \"/var/lib/teslatlas\"\n",
        )
        .expect("default config");
        let options = SetupOptions {
            config_path: config_path.clone(),
            lan_address: Some("192.168.50.20".parse().expect("LAN address")),
            port: 8443,
        };

        let created = configure(&options).expect("create identity");
        assert!(created.created);
        assert_eq!(created.public_url, "https://192.168.50.20:8443");
        assert_eq!(
            fs::metadata(&created.private_key_path)
                .expect("key metadata")
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        let config = HubConfig::load(&config_path).expect("generated config");
        assert_eq!(config.bind, "0.0.0.0:8443".parse().expect("bind"));
        assert_eq!(
            config.tls.expect("TLS").public_url,
            "https://192.168.50.20:8443"
        );

        let reused = configure(&options).expect("reuse identity");
        assert!(!reused.created);
        assert_eq!(reused.certificate_path, created.certificate_path);
        assert_eq!(reused.private_key_path, created.private_key_path);
    }

    #[test]
    fn rejects_loopback_as_a_phone_endpoint() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let error = configure(&SetupOptions {
            config_path: temporary.path().join("config.toml"),
            lan_address: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            port: 8443,
        })
        .expect_err("loopback must fail");
        assert!(matches!(error, SetupError::InvalidLanAddress(_)));
    }
}
