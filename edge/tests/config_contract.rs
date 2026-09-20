#![forbid(unsafe_code)]

use std::fs::{self, OpenOptions};
use std::io::Write;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use tempfile::TempDir;
use teslatlas_edge::config::{ConfigError, EdgeConfig, initialize, rotate_receiver_token};

fn private_file(path: &std::path::Path, bytes: &[u8]) {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).unwrap();
    file.write_all(bytes).unwrap();
}

fn config_text(temp: &TempDir) -> String {
    let root = temp.path();
    format!(
        r#"version = 1
state_directory = "{}"
receiver_bind = "127.0.0.1:18080"
hub_bind = "0.0.0.0:18443"
receiver_bearer_path = "{}"
spool_key_path = "{}"
credential_store_path = "{}"
hub_server_certificate_path = "{}"
hub_server_private_key_path = "{}"
hub_client_ca_path = "{}"

[spool]
max_bytes = 536870912
max_records = 100000
retention_seconds = 604800
batch_max_bytes = 1048576
batch_max_records = 256
"#,
        root.display(),
        root.join("receiver-token").display(),
        root.join("spool-key").display(),
        root.join("credentials.json").display(),
        root.join("server.crt").display(),
        root.join("server.key").display(),
        root.join("hub-ca.crt").display(),
    )
}

fn create_runtime_files(temp: &TempDir) {
    private_file(
        &temp.path().join("receiver-token"),
        b"receiver-token-0123456789",
    );
    private_file(&temp.path().join("spool-key"), &[0x42; 32]);
    private_file(
        &temp.path().join("credentials.json"),
        b"{\"version\":1,\"credentials\":[]}",
    );
    private_file(&temp.path().join("server.key"), b"test-key");
    fs::write(temp.path().join("server.crt"), b"test-cert").unwrap();
    fs::write(temp.path().join("hub-ca.crt"), b"test-ca").unwrap();
}

#[test]
fn strict_configuration_rejects_unknown_keys_and_unsafe_network_shapes() {
    let temp = TempDir::new().unwrap();
    let valid = config_text(&temp);
    assert_eq!(
        EdgeConfig::from_toml(format!("{valid}\nunknown = true\n").as_bytes()).unwrap_err(),
        ConfigError::InvalidToml
    );

    let public_receiver = valid.replace("127.0.0.1:18080", "0.0.0.0:18080");
    assert_eq!(
        EdgeConfig::from_toml(public_receiver.as_bytes()).unwrap_err(),
        ConfigError::UnsafeBind
    );
    let loopback_hub = valid.replace("0.0.0.0:18443", "127.0.0.1:18443");
    assert_eq!(
        EdgeConfig::from_toml(loopback_hub.as_bytes()).unwrap_err(),
        ConfigError::UnsafeBind
    );
    let collision = valid.replace("0.0.0.0:18443", "127.0.0.1:18080");
    assert_eq!(
        EdgeConfig::from_toml(collision.as_bytes()).unwrap_err(),
        ConfigError::UnsafeBind
    );
    let unbounded = valid.replace("max_records = 100000", "max_records = 1000001");
    assert_eq!(
        EdgeConfig::from_toml(unbounded.as_bytes()).unwrap_err(),
        ConfigError::InvalidLimit
    );
}

#[test]
fn local_source_run_loopback_hub_bind_is_explicit_and_fail_closed() {
    let temp = TempDir::new().unwrap();
    let valid = config_text(&temp);
    let loopback_hub = valid.replace("0.0.0.0:18443", "127.0.0.1:18443");
    let opted_in_loopback_hub = loopback_hub.replace(
        "hub_bind = \"127.0.0.1:18443\"",
        "hub_bind = \"127.0.0.1:18443\"\nallow_local_source_run_hub_loopback = true",
    );
    let local_source_run = EdgeConfig::from_toml(opted_in_loopback_hub.as_bytes()).unwrap();
    assert!(local_source_run.hub_bind.ip().is_loopback());
    assert!(local_source_run.allow_local_source_run_hub_loopback);

    let stray_loopback_opt_in = valid.replace(
        "hub_bind = \"0.0.0.0:18443\"",
        "hub_bind = \"0.0.0.0:18443\"\nallow_local_source_run_hub_loopback = true",
    );
    assert_eq!(
        EdgeConfig::from_toml(stray_loopback_opt_in.as_bytes()).unwrap_err(),
        ConfigError::UnsafeBind
    );

    let public_receiver = opted_in_loopback_hub.replace("127.0.0.1:18080", "0.0.0.0:18080");
    assert_eq!(
        EdgeConfig::from_toml(public_receiver.as_bytes()).unwrap_err(),
        ConfigError::UnsafeBind
    );

    let colliding_loopback_ports = opted_in_loopback_hub.replace(
        "hub_bind = \"127.0.0.1:18443\"",
        "hub_bind = \"127.0.0.1:18080\"",
    );
    assert_eq!(
        EdgeConfig::from_toml(colliding_loopback_ports.as_bytes()).unwrap_err(),
        ConfigError::UnsafeBind
    );
}

#[test]
fn configuration_rejects_colliding_runtime_paths_before_initialization() {
    let temp = TempDir::new().unwrap();
    let valid = config_text(&temp);
    let receiver_path = temp.path().join("receiver-token").display().to_string();
    let colliding = valid.replace(
        &temp.path().join("spool-key").display().to_string(),
        &receiver_path,
    );

    assert_eq!(
        EdgeConfig::from_toml(colliding.as_bytes()).unwrap_err(),
        ConfigError::InvalidPath
    );
}

#[test]
fn configuration_rejects_a_runtime_file_path_equal_to_the_state_directory() {
    let temp = TempDir::new().unwrap();
    let valid = config_text(&temp);
    let old_state = format!("state_directory = \"{}\"", temp.path().display());
    let colliding_state = format!(
        "state_directory = \"{}\"",
        temp.path().join("receiver-token").display()
    );
    let colliding = valid.replacen(&old_state, &colliding_state, 1);

    assert_eq!(
        EdgeConfig::from_toml(colliding.as_bytes()).unwrap_err(),
        ConfigError::InvalidPath
    );
}

#[test]
fn configuration_rejects_relative_and_symlinked_secret_paths() {
    let temp = TempDir::new().unwrap();
    create_runtime_files(&temp);
    let relative = config_text(&temp).replace(
        &temp.path().join("receiver-token").display().to_string(),
        "receiver-token",
    );
    assert_eq!(
        EdgeConfig::from_toml(relative.as_bytes()).unwrap_err(),
        ConfigError::InvalidPath
    );

    #[cfg(unix)]
    {
        let link = temp.path().join("linked-token");
        symlink(temp.path().join("receiver-token"), &link).unwrap();
        let linked = config_text(&temp).replace(
            &temp.path().join("receiver-token").display().to_string(),
            &link.display().to_string(),
        );
        assert_eq!(
            EdgeConfig::from_toml(linked.as_bytes())
                .unwrap()
                .validate_runtime_files()
                .unwrap_err(),
            ConfigError::InvalidPath
        );

        let linked_parent = temp.path().join("linked-parent");
        symlink(temp.path(), &linked_parent).unwrap();
        let parent_linked = config_text(&temp).replace(
            &temp.path().join("receiver-token").display().to_string(),
            &linked_parent.join("receiver-token").display().to_string(),
        );
        assert_eq!(
            EdgeConfig::from_toml(parent_linked.as_bytes())
                .unwrap()
                .validate_runtime_files()
                .unwrap_err(),
            ConfigError::InvalidPath
        );
    }
}

#[cfg(unix)]
#[test]
fn initialization_rejects_a_symlinked_parent_of_the_state_directory() {
    let temp = TempDir::new().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    private_file(&temp.path().join("server.key"), b"test-key");
    fs::write(temp.path().join("server.crt"), b"test-cert").unwrap();
    fs::write(temp.path().join("hub-ca.crt"), b"test-ca").unwrap();

    let real_parent = temp.path().join("real-state-parent");
    fs::create_dir(&real_parent).unwrap();
    fs::set_permissions(&real_parent, fs::Permissions::from_mode(0o700)).unwrap();
    let linked_parent = temp.path().join("linked-state-parent");
    symlink(&real_parent, &linked_parent).unwrap();
    let linked_state = linked_parent.join("edge-state");
    let valid = config_text(&temp);
    let old_state = format!("state_directory = \"{}\"", temp.path().display());
    let linked = format!("state_directory = \"{}\"", linked_state.display());
    let config = EdgeConfig::from_toml(valid.replacen(&old_state, &linked, 1).as_bytes()).unwrap();

    assert_eq!(initialize(&config).unwrap_err(), ConfigError::InvalidPath);
    assert!(!real_parent.join("edge-state").exists());
    assert!(!config.receiver_bearer_path.exists());
    assert!(!config.spool_key_path.exists());
    assert!(!config.credential_store_path.exists());
}

#[test]
fn runtime_file_validation_requires_present_private_regular_secrets() {
    let temp = TempDir::new().unwrap();
    create_runtime_files(&temp);
    let config = EdgeConfig::from_toml(config_text(&temp).as_bytes()).unwrap();
    config.validate_runtime_files().unwrap();

    #[cfg(unix)]
    {
        fs::set_permissions(
            temp.path().join("server.key"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert_eq!(
            config.validate_runtime_files().unwrap_err(),
            ConfigError::UnsafePermissions
        );
    }

    fs::remove_file(temp.path().join("server.key")).unwrap();
    assert_eq!(
        config.validate_runtime_files().unwrap_err(),
        ConfigError::MissingFile
    );
}

#[test]
fn initialization_creates_only_private_edge_secrets_and_never_overwrites() {
    let temp = TempDir::new().unwrap();
    #[cfg(unix)]
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    private_file(&temp.path().join("server.key"), b"test-key");
    fs::write(temp.path().join("server.crt"), b"test-cert").unwrap();
    fs::write(temp.path().join("hub-ca.crt"), b"test-ca").unwrap();
    let config = EdgeConfig::from_toml(config_text(&temp).as_bytes()).unwrap();

    initialize(&config).unwrap();
    assert_eq!(fs::read(&config.spool_key_path).unwrap().len(), 32);
    let receiver_token = fs::read_to_string(&config.receiver_bearer_path).unwrap();
    assert!(receiver_token.len() >= 32);
    assert!(receiver_token.is_ascii());
    assert_eq!(
        fs::read_to_string(&config.credential_store_path).unwrap(),
        "{\n  \"version\": 1,\n  \"credentials\": []\n}"
    );
    #[cfg(unix)]
    for path in [
        &config.receiver_bearer_path,
        &config.spool_key_path,
        &config.credential_store_path,
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        initialize(&config).unwrap_err(),
        ConfigError::AlreadyInitialized
    );
}

#[test]
fn receiver_token_rotation_is_atomic_private_and_returns_the_new_value_once() {
    let temp = TempDir::new().unwrap();
    #[cfg(unix)]
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    private_file(&temp.path().join("server.key"), b"test-key");
    fs::write(temp.path().join("server.crt"), b"test-cert").unwrap();
    fs::write(temp.path().join("hub-ca.crt"), b"test-ca").unwrap();
    let config = EdgeConfig::from_toml(config_text(&temp).as_bytes()).unwrap();
    initialize(&config).unwrap();
    let old = fs::read_to_string(&config.receiver_bearer_path).unwrap();

    let issued = rotate_receiver_token(&config).unwrap();
    assert_ne!(issued.token(), old);
    assert_eq!(
        fs::read_to_string(&config.receiver_bearer_path).unwrap(),
        issued.token()
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&config.receiver_bearer_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn initialization_never_chmods_an_existing_public_state_directory() {
    let temp = TempDir::new().unwrap();
    let public_state = temp.path().join("public-state");
    fs::create_dir(&public_state).unwrap();
    fs::set_permissions(&public_state, fs::Permissions::from_mode(0o755)).unwrap();
    let text = config_text(&temp).replace(
        &format!("state_directory = \"{}\"", temp.path().display()),
        &format!("state_directory = \"{}\"", public_state.display()),
    );
    let config = EdgeConfig::from_toml(text.as_bytes()).unwrap();
    assert_eq!(
        initialize(&config).unwrap_err(),
        ConfigError::UnsafePermissions
    );
    assert_eq!(
        fs::metadata(public_state).unwrap().permissions().mode() & 0o777,
        0o755
    );
}
