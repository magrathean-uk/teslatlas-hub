// SPDX-License-Identifier: AGPL-3.0-only

use std::{os::unix::fs::PermissionsExt, process::Command, sync::mpsc, thread, time::Duration};

use super::*;

fn valid_config(data_dir: &Path) -> String {
    format!(
        "data_dir = '{}'\nbind = '127.0.0.1:8080'\n",
        data_dir.display()
    )
}

#[test]
fn cors_configuration_accepts_only_canonical_explicit_origins() {
    let base = valid_config(Path::new("/var/lib/teslatlas"));
    let allowed = format!(
        "{base}[http]\nallowed_origins = ['https://client.example', 'http://localhost:5173']\n"
    );
    let result = HubConfig::from_exact_bytes(allowed.as_bytes());
    assert!(
        result.is_ok(),
        "explicit browser origins must be supported: {result:?}"
    );
    for origin in [
        "*",
        "null",
        "https://client.example/",
        "https://client.example/path",
        "https://user@client.example",
        "https://client.example?token=value",
        "https://client.example#fragment",
        "file://client",
        "https://client.example:443",
        "HTTPS://CLIENT.EXAMPLE",
    ] {
        let config = format!("{base}[http]\nallowed_origins = ['{origin}']\n");
        assert!(
            HubConfig::from_exact_bytes(config.as_bytes()).is_err(),
            "accepted invalid origin {origin}"
        );
    }
    let duplicate = format!(
        "{base}[http]\nallowed_origins = ['https://client.example', 'https://client.example']\n"
    );
    assert!(HubConfig::from_exact_bytes(duplicate.as_bytes()).is_err());
}

#[test]
fn pairing_lifetime_defaults_to_180_days_and_is_limited_to_1_through_3650() {
    let base = valid_config(Path::new("/var/lib/teslatlas"));
    let parse = |table: &str| HubConfig::from_exact_bytes(format!("{base}{table}").as_bytes());

    // No table, an empty table and an explicit 180 are the one default, and
    // it is the span the store writes without configuration.
    for table in [
        "",
        "[pairing]\n",
        "[pairing]\ndevice_max_lifetime_days = 180\n",
    ] {
        let (config, _) = parse(table).expect("the default lifetime");
        assert_eq!(config.pairing, PairingConfig::default(), "{table:?}");
        assert_eq!(config.pairing.device_max_lifetime_days, 180);
        assert_eq!(
            config.pairing.device_max_lifetime_ms(),
            crate::db::PAIRED_DEVICE_MAX_LIFETIME_MS
        );
    }
    for days in [1_u32, 2, 30, 365, 3650] {
        let (config, _) = parse(&format!("[pairing]\ndevice_max_lifetime_days = {days}\n"))
            .unwrap_or_else(|error| panic!("{days} days: {error}"));
        assert_eq!(config.pairing.device_max_lifetime_days, days);
        assert_eq!(
            config.pairing.device_max_lifetime_ms(),
            i64::from(days) * 86_400_000
        );
    }
    // Out of range is a validation error naming the key, not a parse error.
    for days in ["0", "3651", "4294967295"] {
        match parse(&format!("[pairing]\ndevice_max_lifetime_days = {days}\n")) {
            Err(ConfigError::Invalid(message)) => {
                assert!(
                    message.contains("pairing.device_max_lifetime_days"),
                    "{message}"
                );
            }
            other => panic!("{days} days must be refused by validation: {other:?}"),
        }
    }
    // Not an unsigned whole number of days, or not a number.
    for value in ["-1", "1.5", "'180'", "true", "4294967296"] {
        assert!(
            matches!(
                parse(&format!("[pairing]\ndevice_max_lifetime_days = {value}\n")),
                Err(ConfigError::Parse(_))
            ),
            "{value}"
        );
    }
    // The table takes this key and no other.
    assert!(matches!(
        parse("[pairing]\ndevice_max_lifetime = 180\n"),
        Err(ConfigError::Parse(_))
    ));
    assert!(matches!(
        parse("[pairing]\ndevice_max_lifetime_days = 180\nunknown = 1\n"),
        Err(ConfigError::Parse(_))
    ));
    // Validation is also what the serve path relies on when it builds a
    // config by hand.
    let mut config = HubConfig::from_exact_bytes(base.as_bytes()).unwrap().0;
    config.pairing.device_max_lifetime_days = 0;
    assert!(matches!(config.validate(), Err(ConfigError::Invalid(_))));
    assert_eq!(
        PairingConfig {
            device_max_lifetime_days: u32::MAX
        }
        .device_max_lifetime_ms(),
        i64::from(u32::MAX) * 86_400_000
    );
}

#[test]
fn load_rejects_oversized_symlinked_and_wrong_mode_config_files() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().expect("temporary directory");
    let data_dir = temporary.path().join("data");
    let path = temporary.path().join("config.toml");
    fs::write(&path, valid_config(&data_dir)).expect("config");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).expect("unsafe mode");
    assert!(matches!(
        HubConfig::load(&path),
        Err(ConfigError::UnsafeFile)
    ));

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("safe mode");
    let linked = temporary.path().join("linked.toml");
    symlink(&path, &linked).expect("config symlink");
    assert!(matches!(HubConfig::load(&linked), Err(ConfigError::Read)));

    fs::write(&path, vec![b'x'; MAX_CONFIG_BYTES + 1]).expect("oversized config");
    assert!(matches!(HubConfig::load(&path), Err(ConfigError::TooLarge)));
}

#[test]
fn load_rejects_config_path_replacement_after_open() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data_dir = temporary.path().join("data");
    let path = temporary.path().join("config.toml");
    fs::write(&path, valid_config(&data_dir)).expect("config");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("safe mode");
    let replacement = temporary.path().join("replacement.toml");
    fs::write(&replacement, valid_config(&data_dir)).expect("replacement");
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).expect("replacement mode");

    assert!(matches!(
        read_config_file_after_open(&path, || fs::rename(&replacement, &path).expect("replace")),
        Err(ConfigError::FileIdentityChanged)
    ));
}

#[test]
fn load_rejects_a_fifo_without_waiting_for_a_writer() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let path = temporary.path().join("config.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("run mkfifo")
            .success()
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("FIFO mode");

    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        sender
            .send(matches!(
                HubConfig::load(path),
                Err(ConfigError::UnsafeFile)
            ))
            .expect("send FIFO result");
    });
    assert!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("FIFO admission must not block")
    );
    worker.join().expect("FIFO admission worker");
}

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
fn enrichment_defaults_are_private() {
    assert!(!TerrainConfig::default().enabled);
    assert_eq!(TerrainConfig::default().max_cache_bytes, 512 * 1024 * 1024);
    assert!(!GeocoderConfig::default().enabled);
    assert!(GeocoderConfig::default().endpoint.is_none());
}

#[test]
fn enabled_geocoder_requires_an_explicit_provider_endpoint() {
    let error = HubConfig::from_exact_bytes(
        b"data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
          [geocoder]\nenabled = true\n",
    )
    .expect_err("enabled geocoder without endpoint");
    assert!(matches!(error, ConfigError::InvalidGeocoderEndpoint));
}

#[test]
fn terrain_cache_must_hold_one_srtm1_tile() {
    let config = TerrainConfig {
        max_cache_bytes: crate::terrain::SRTM1_BYTES,
        ..TerrainConfig::default()
    };
    assert!(matches!(
        config.validate(),
        Err(ConfigError::InvalidTerrainConfig)
    ));
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
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .expect("make test config private");
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
    collector.stream_endpoint_override = Some("ws://stream.example/?refresh-secret=1".to_owned());
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
fn fleet_telemetry_requires_fleet_proxy_and_safe_paths() {
    let valid = "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
                 [collector]\nprovider = 'fleet'\n\
                 fleet_command_proxy_url = 'https://127.0.0.1:4443'\n\
                 [collector.fleet_telemetry]\n\
                 hostname = 'telemetry.example.test'\n\
                 ca_certificate_path = '/etc/teslatlas-hub/telemetry/ca.pem'\n\
                 ingest_token_path = '/var/lib/teslatlas-hub/secrets/telemetry-token'\n";
    HubConfig::from_exact_bytes(valid.as_bytes()).expect("valid Fleet Telemetry config");

    for invalid in [
        valid.replace("provider = 'fleet'\n", ""),
        valid.replace("fleet_command_proxy_url = 'https://127.0.0.1:4443'\n", ""),
        valid.replace("127.0.0.1:8080", "127.0.0.1:8081"),
        valid.replace("127.0.0.1:8080", "[::1]:8080"),
        valid.replace("telemetry.example.test", "https://telemetry.example.test"),
        valid.replace(
            "/var/lib/teslatlas-hub/secrets/telemetry-token",
            "relative-token",
        ),
    ] {
        assert!(matches!(
            HubConfig::from_exact_bytes(invalid.as_bytes()),
            Err(ConfigError::InvalidFleetTelemetry)
        ));
    }
}

#[test]
fn fleet_telemetry_accepts_public_tls_with_a_private_loopback_ingress() {
    let loopback = "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
                 [collector]\nprovider = 'fleet'\n\
                 fleet_command_proxy_url = 'https://127.0.0.1:4443'\n\
                 [collector.fleet_telemetry]\n\
                 hostname = 'telemetry.example.test'\n\
                 ca_certificate_path = '/etc/teslatlas-hub/telemetry/ca.pem'\n\
                 ingest_token_path = '/var/lib/teslatlas-hub/secrets/telemetry-token'\n";

    for bind in ["127.0.0.1:8443", "0.0.0.0:8443", "192.0.2.10:8080"] {
        let config = format!(
            "{}[tls]\ncertificate_path = '/etc/teslatlas/tls/cert.pem'\n\
             private_key_path = '/etc/teslatlas/tls/key.pem'\n\
             public_url = 'https://hub.example.test'\n",
            loopback.replace("127.0.0.1:8080", bind)
        );
        HubConfig::from_exact_bytes(config.as_bytes()).unwrap_or_else(|error| {
            panic!("public TLS Fleet bind {bind} must be accepted: {error}")
        });
    }
}

#[test]
fn fleet_telemetry_rejects_a_public_tls_listener_collision_with_private_ingress() {
    let loopback = "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
                 [collector]\nprovider = 'fleet'\n\
                 fleet_command_proxy_url = 'https://127.0.0.1:4443'\n\
                 [collector.fleet_telemetry]\n\
                 hostname = 'telemetry.example.test'\n\
                 ca_certificate_path = '/etc/teslatlas-hub/telemetry/ca.pem'\n\
                 ingest_token_path = '/var/lib/teslatlas-hub/secrets/telemetry-token'\n";

    for bind in ["127.0.0.1:8080", "0.0.0.0:8080", "[::]:8080"] {
        let config = format!(
            "{}[tls]\ncertificate_path = '/etc/teslatlas/tls/cert.pem'\n\
             private_key_path = '/etc/teslatlas/tls/key.pem'\n\
             public_url = 'https://hub.example.test'\n",
            loopback.replace("127.0.0.1:8080", bind)
        );
        assert!(matches!(
            HubConfig::from_exact_bytes(config.as_bytes()),
            Err(ConfigError::FleetTelemetryListenerCollision)
        ));
    }
}

#[test]
fn fleet_telemetry_still_rejects_public_plaintext() {
    let config = "data_dir = '/var/lib/teslatlas'\nbind = '0.0.0.0:8443'\n\
                  [collector]\nprovider = 'fleet'\n\
                  fleet_command_proxy_url = 'https://127.0.0.1:4443'\n\
                  [collector.fleet_telemetry]\n\
                  hostname = 'telemetry.example.test'\n\
                  ca_certificate_path = '/etc/teslatlas-hub/telemetry/ca.pem'\n\
                  ingest_token_path = '/var/lib/teslatlas-hub/secrets/telemetry-token'\n";
    assert!(matches!(
        HubConfig::from_exact_bytes(config.as_bytes()),
        Err(ConfigError::NonLoopbackBind)
    ));
}

#[test]
fn fleet_telemetry_debug_redacts_secret_path() {
    let telemetry = FleetTelemetryConfig {
        hostname: "telemetry.example.test".to_owned(),
        port: 443,
        ca_certificate_path: PathBuf::from("/public/ca.pem"),
        ingest_token_path: PathBuf::from("/secret/marker-token"),
    };
    let rendered = format!("{telemetry:?}");
    assert!(rendered.contains("telemetry.example.test"));
    assert!(!rendered.contains("marker-token"));
    assert!(!rendered.contains("ca.pem"));
}

#[test]
fn edge_collector_is_opt_in_and_exclusive_with_direct_ingestion() {
    let valid = "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
                 [collector]\nprovider = 'fleet'\ninterval_seconds = 0\n\
                 [collector.edge]\n\
                 base_url = 'https://edge.example.test:18470/'\n\
                 ca_certificate_path = '/etc/teslatlas-hub/edge/ca.pem'\n\
                 client_certificate_path = '/etc/teslatlas-hub/edge/client.pem'\n\
                 client_private_key_path = '/var/lib/teslatlas-hub/secrets/edge-key.pem'\n\
                 bearer_token_path = '/var/lib/teslatlas-hub/secrets/edge-token'\n\
                 installation_id = 'garage-edge'\nlineage = 'spool-2026-09-05'\n\
                 source_id = '11111111-1111-4111-8111-111111111111'\n\
                 vehicle_id = '22222222-2222-4222-8222-222222222222'\n\
                 vin = '5YJ3E1EA7KF000001'\ncar_id = 101\n";
    let (config, _) = HubConfig::from_exact_bytes(valid.as_bytes()).expect("valid Edge config");
    assert!(config.collector.edge.is_some());

    for invalid in [
        valid.replace("provider = 'fleet'\n", ""),
        valid.replace("interval_seconds = 0", "interval_seconds = 60"),
        valid.replace(
            "https://edge.example.test:18470/",
            "http://edge.example.test:18470/",
        ),
        valid.replace("garage-edge", "Garage Edge"),
        valid.replace(
            "/var/lib/teslatlas-hub/secrets/edge-token",
            "relative-token",
        ),
    ] {
        assert!(matches!(
            HubConfig::from_exact_bytes(invalid.as_bytes()),
            Err(ConfigError::InvalidEdge)
        ));
    }
}

#[test]
fn edge_collector_debug_redacts_private_paths() {
    let edge = EdgeCollectorConfig {
        base_url: "https://edge.example.test:18470/".to_owned(),
        ca_certificate_path: PathBuf::from("/public/ca-marker.pem"),
        client_certificate_path: PathBuf::from("/public/client-marker.pem"),
        client_private_key_path: PathBuf::from("/secret/key-marker.pem"),
        bearer_token_path: PathBuf::from("/secret/token-marker"),
        installation_id: "garage-edge".to_owned(),
        lineage: "spool-2026-09-05".to_owned(),
        source_id: uuid::Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        vehicle_id: uuid::Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
        vin: "5YJ3E1EA7KF000001".to_owned(),
        car_id: 101,
        poll_milliseconds: 500,
        timeout_seconds: 20,
        max_backoff_seconds: 60,
    };
    let rendered = format!("{edge:?}");
    for marker in ["ca-marker", "client-marker", "key-marker", "token-marker"] {
        assert!(!rendered.contains(marker));
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

// ---------------------------------------------------------------------------
// C3 part K1: collector defaults, validation and modes (CT1 to CT3, CT58).
// ---------------------------------------------------------------------------

const PROVIDER_BASE: &str = "https://api.provider.example.test";

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn with_mode(mode: CollectorMode, mut config: CollectorConfig) -> CollectorConfig {
    match mode {
        CollectorMode::Legacy => {}
        CollectorMode::Provider => config.owner_api_base_url = Some(PROVIDER_BASE.to_owned()),
        CollectorMode::FleetPolling => config.provider = CollectorProvider::Fleet,
    }
    config
}

const ALL_MODES: [CollectorMode; 3] = [
    CollectorMode::Legacy,
    CollectorMode::Provider,
    CollectorMode::FleetPolling,
];

#[test]
fn ct1_collector_defaults_follow_the_cadence_tables() {
    let config = CollectorConfig::default();
    assert_eq!(config.driving_poll_milliseconds, 10_000);
    assert_eq!(config.driving_stream_poll_seconds, 60);
    assert_eq!(config.charging_poll_seconds, 30);
    assert_eq!(config.updating_poll_seconds, 120);
    assert_eq!(config.online_poll_seconds, 60);
    assert_eq!(config.suspended_poll_seconds, 30);
    assert_eq!(config.sleeping_poll_seconds, 30);
    assert_eq!(config.interval_seconds, 300);
    assert_eq!(config.offline_drive_timeout_seconds, 600);
    assert_eq!(config.max_backoff_seconds, 300);
    assert_eq!(config.request_timeout_seconds, 15);
    assert_eq!(config.stream_health_timeout_seconds, 60);
    assert_eq!(config.idle_suspend_after_seconds, 900);
    assert_eq!(collector_mode(&config), CollectorMode::Legacy);
    assert!(config.start_notices().is_empty());

    let cadence = config.cadence().expect("default cadence is valid");
    assert_eq!(cadence.driving, Duration::from_millis(10_000));
    assert_eq!(cadence.driving_stream, secs(60));
    assert_eq!(cadence.charging, secs(30));
    assert_eq!(cadence.updating, secs(120));
    assert_eq!(cadence.online, secs(60));
    assert_eq!(cadence.suspended, secs(30));
    assert_eq!(cadence.sleeping, secs(30));
    assert_eq!(cadence.inventory, secs(300));
    assert_eq!(cadence.offline_drive_timeout, secs(600));
    assert_eq!(cadence.maximum_backoff, secs(300));
    assert_eq!(cadence.stream_health_timeout, secs(60));
    // The configured silence tolerance is the lifecycle's own default.
    assert_eq!(
        cadence.offline_drive_timeout,
        crate::lifecycle::DEFAULT_OFFLINE_DRIVE_TIMEOUT
    );
    assert_eq!(cadence.fleet, FleetCadence::DEFAULT);
}

#[test]
fn ct1_fleet_polling_table_defaults_depend_on_whether_state_checks_are_billed() {
    // An absent table and an empty table are the same thing.
    let absent: HubConfig =
        toml::from_str("data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'").unwrap();
    let empty: HubConfig = toml::from_str(
        "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n[collector.fleet_polling]\n",
    )
    .unwrap();
    assert_eq!(
        absent.collector.fleet_polling,
        empty.collector.fleet_polling
    );

    let cadence = absent.collector.cadence().unwrap();
    assert!(FLEET_PRICING.state_billed());
    let billed = cadence.fleet_policy();
    assert_eq!(billed.pricing, FLEET_PRICING);
    assert_eq!(
        billed.intervals,
        FleetIntervals {
            driving: secs(30),
            charging: secs(120),
            updating: secs(300),
            online: secs(120),
            state_check: secs(600),
            inventory: secs(3_600),
        }
    );
    assert_eq!(billed.monthly_cap_cents, 1_000);

    let free_pricing = FleetPricing {
        state: None,
        ..FLEET_PRICING
    };
    let free = FleetPolicy::resolve(&cadence.fleet, free_pricing);
    assert!(!free_pricing.state_billed());
    assert_eq!(free.intervals.state_check, secs(30));
    assert_eq!(free.intervals.inventory, secs(300));
    assert_eq!(free.intervals.driving, secs(30));
    assert_eq!(free.monthly_cap_cents, 1_000);
}

#[test]
fn ct1_published_fleet_rates_are_the_ones_of_2026_10_11() {
    assert_eq!(
        FLEET_PRICING,
        FleetPricing {
            data: 500,
            state: Some(500),
            wake: 50,
            command: 1_000,
            signal: 150_000,
        }
    );
}

#[test]
fn ct1_an_owner_set_fleet_state_interval_wins_over_the_pricing_default() {
    let config: HubConfig = toml::from_str(
        "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
         [collector]\nprovider = 'fleet'\n\
         [collector.fleet_polling]\ndriving_poll_seconds = 20\n\
         state_poll_seconds = 90\ninventory_poll_seconds = 7200\nmonthly_cap_cents = 0\n",
    )
    .unwrap();
    config.validate().unwrap();
    assert_eq!(
        collector_mode(&config.collector),
        CollectorMode::FleetPolling
    );
    let policy = config.collector.cadence().unwrap().fleet_policy();
    assert_eq!(policy.intervals.driving, secs(20));
    assert_eq!(policy.intervals.state_check, secs(90));
    assert_eq!(policy.intervals.inventory, secs(7_200));
    assert_eq!(policy.monthly_cap_cents, 0);
}

/// The first violated cadence rule of a collector configuration, as the text
/// the owner reads.
fn first_violation(config: &CollectorConfig) -> Option<String> {
    config
        .cadence_violation()
        .map(|violation| violation.to_string())
}

#[test]
fn ct2_session_intervals_must_stay_below_the_silence_tolerance_and_the_energy_bound() {
    let base = CollectorConfig::default();
    let bound = "must be below 600 seconds, the longest charging interval the Hub integrates";

    // The Owner-wire charging interval against `K_INT_MAX_MS`, 599 and 600 s,
    // with the silence tolerance at 600 s and at 900 s, in every mode.
    let charging = |seconds, timeout| CollectorConfig {
        charging_poll_seconds: seconds,
        offline_drive_timeout_seconds: timeout,
        ..base.clone()
    };
    for mode in ALL_MODES {
        for timeout in [600, 900] {
            assert_eq!(
                first_violation(&with_mode(mode, charging(600, timeout))),
                Some(format!(
                    "collector setting collector.charging_poll_seconds = 600 {bound}"
                )),
                "owner-wire charging 600 s, T_gap {timeout} s, {mode:?}"
            );
        }
        assert!(
            with_mode(mode, charging(599, 900)).cadence().is_ok(),
            "owner-wire charging 599 s with T_gap 900 s in {mode:?}"
        );
    }
    // 599 below a 600 s tolerance is valid for the energy bound but not for the
    // silence tolerance in Owner-wire mode, where the interval is a session one:
    // the relative rule, not the integration bound, reports it.
    assert_eq!(
        first_violation(&charging(599, 599)),
        Some(
            "collector setting collector.charging_poll_seconds = 599 must be below \
             collector.offline_drive_timeout_seconds (599)"
                .to_owned()
        )
    );

    // The Fleet charging key is checked in every mode, with its own key.
    let fleet_charging = |seconds, timeout| CollectorConfig {
        fleet_polling: FleetPollingConfig {
            charging_poll_seconds: seconds,
            ..FleetPollingConfig::default()
        },
        offline_drive_timeout_seconds: timeout,
        ..base.clone()
    };
    for mode in ALL_MODES {
        assert_eq!(
            first_violation(&with_mode(mode, fleet_charging(600, 900))),
            Some(format!(
                "collector setting collector.fleet_polling.charging_poll_seconds = 600 {bound}"
            )),
            "fleet charging 600 s with T_gap 900 s in {mode:?}"
        );
        assert!(
            with_mode(mode, fleet_charging(599, 900)).cadence().is_ok(),
            "fleet charging 599 s with T_gap 900 s in {mode:?}"
        );
    }
    assert_eq!(
        first_violation(&with_mode(
            CollectorMode::FleetPolling,
            fleet_charging(600, 600)
        )),
        Some(format!(
            "collector setting collector.fleet_polling.charging_poll_seconds = 600 {bound}"
        ))
    );
    // Both keys at 600: the Owner-wire one is reported first.
    let both = CollectorConfig {
        charging_poll_seconds: 600,
        fleet_polling: FleetPollingConfig {
            charging_poll_seconds: 600,
            ..FleetPollingConfig::default()
        },
        offline_drive_timeout_seconds: 900,
        ..base
    };
    assert_eq!(
        both.cadence_violation().map(|violation| violation.key),
        Some("collector.charging_poll_seconds")
    );
}

#[test]
fn ct2_each_positive_cadence_key_reports_its_own_error() {
    let base = CollectorConfig::default();
    type Zero = fn(&mut CollectorConfig);
    let zeroes: [(&str, Zero); 15] = [
        ("collector.fleet_polling.driving_poll_seconds", |c| {
            c.fleet_polling.driving_poll_seconds = 0
        }),
        ("collector.fleet_polling.charging_poll_seconds", |c| {
            c.fleet_polling.charging_poll_seconds = 0
        }),
        ("collector.fleet_polling.updating_poll_seconds", |c| {
            c.fleet_polling.updating_poll_seconds = 0
        }),
        ("collector.fleet_polling.online_poll_seconds", |c| {
            c.fleet_polling.online_poll_seconds = 0
        }),
        ("collector.fleet_polling.state_poll_seconds", |c| {
            c.fleet_polling.state_poll_seconds = Some(0)
        }),
        ("collector.fleet_polling.inventory_poll_seconds", |c| {
            c.fleet_polling.inventory_poll_seconds = Some(0)
        }),
        ("collector.driving_poll_milliseconds", |c| {
            c.driving_poll_milliseconds = 0
        }),
        ("collector.driving_stream_poll_seconds", |c| {
            c.driving_stream_poll_seconds = 0
        }),
        ("collector.charging_poll_seconds", |c| {
            c.charging_poll_seconds = 0
        }),
        ("collector.online_poll_seconds", |c| {
            c.online_poll_seconds = 0
        }),
        ("collector.sleeping_poll_seconds", |c| {
            c.sleeping_poll_seconds = 0
        }),
        ("collector.offline_drive_timeout_seconds", |c| {
            c.offline_drive_timeout_seconds = 0
        }),
        ("collector.suspended_poll_seconds", |c| {
            c.suspended_poll_seconds = 0
        }),
        ("collector.updating_poll_seconds", |c| {
            c.updating_poll_seconds = 0
        }),
        ("collector.max_backoff_seconds", |c| {
            c.max_backoff_seconds = 0
        }),
    ];
    for mode in ALL_MODES {
        for (key, zero) in zeroes {
            let mut config = with_mode(mode, base.clone());
            zero(&mut config);
            let violation = config
                .cadence_violation()
                .unwrap_or_else(|| panic!("{key} {mode:?}"));
            assert_eq!(violation.key, key, "{mode:?}");
            assert_eq!(violation.value, 0, "{key}");
            assert_eq!(violation.rule, CadenceRule::GreaterThanZero, "{key}");
            assert_eq!(
                violation.to_string(),
                format!("collector setting {key} = 0 must be greater than zero")
            );
            // `cadence()` carries the same payload and message.
            match config.cadence() {
                Err(ConfigError::InvalidCollectorCadence(carried)) => {
                    assert_eq!(carried, violation);
                    assert_eq!(
                        ConfigError::InvalidCollectorCadence(carried).to_string(),
                        violation.to_string()
                    );
                }
                other => panic!("{key}: {other:?}"),
            }
        }
    }
    // An absent optional key passes; the unsigned cap of zero is valid; an
    // `interval_seconds` of zero only disables collection; the health timeout is
    // clamped rather than rejected.
    let mut config = base;
    config.fleet_polling.state_poll_seconds = None;
    config.fleet_polling.inventory_poll_seconds = None;
    config.fleet_polling.monthly_cap_cents = 0;
    config.interval_seconds = 0;
    config.stream_health_timeout_seconds = 0;
    assert_eq!(config.cadence_violation(), None);
    assert!(config.cadence().is_ok());
}

#[test]
fn ct2_driving_and_request_timeout_boundaries_report_exact_errors() {
    for mode in ALL_MODES {
        let driving = |milliseconds| {
            with_mode(
                mode,
                CollectorConfig {
                    driving_poll_milliseconds: milliseconds,
                    ..CollectorConfig::default()
                },
            )
        };
        assert_eq!(
            first_violation(&driving(999)),
            Some(
                "collector setting collector.driving_poll_milliseconds = 999 must be at least \
                 1000 milliseconds"
                    .to_owned()
            ),
            "{mode:?}"
        );
        assert_eq!(first_violation(&driving(1_000)), None, "{mode:?}");
        let timeout = |seconds| {
            with_mode(
                mode,
                CollectorConfig {
                    request_timeout_seconds: seconds,
                    ..CollectorConfig::default()
                },
            )
        };
        for seconds in [1, 120] {
            assert_eq!(
                first_violation(&timeout(seconds)),
                None,
                "{seconds} s {mode:?}"
            );
        }
        for seconds in [0, 121] {
            assert_eq!(
                first_violation(&timeout(seconds)),
                Some(format!(
                    "collector setting collector.request_timeout_seconds = {seconds} must be \
                     between 1 and 120 seconds inclusive"
                )),
                "{seconds} s {mode:?}"
            );
        }
    }
    // Loading still refuses a zero timeout, with its own error, and accepts 121,
    // which only the cadence view reports.
    let load = |seconds: u64| {
        let config: HubConfig = toml::from_str(&format!(
            "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
             [collector]\nrequest_timeout_seconds = {seconds}\n"
        ))
        .expect("parse");
        config.validate().map(|()| config)
    };
    assert!(matches!(load(0), Err(ConfigError::InvalidOwnerApiTimeout)));
    let loaded = load(121).expect("a timeout of 121 s still loads");
    assert_eq!(
        loaded
            .collector
            .cadence_violation()
            .map(|violation| violation.key),
        Some("collector.request_timeout_seconds")
    );
}

#[test]
fn ct2_every_active_interval_reports_its_timeout_bound() {
    // With the silence tolerance at 120 s, each active interval is isolated: one
    // below is valid, one at it reports its own key, value and the bound, and the
    // interval of the other mode is never judged.
    let tolerant = |change: &dyn Fn(&mut CollectorConfig)| {
        let mut config = CollectorConfig {
            offline_drive_timeout_seconds: 120,
            max_backoff_seconds: 100,
            fleet_polling: FleetPollingConfig {
                driving_poll_seconds: 30,
                charging_poll_seconds: 60,
                online_poll_seconds: 60,
                ..FleetPollingConfig::default()
            },
            ..CollectorConfig::default()
        };
        change(&mut config);
        config
    };
    let relative = |key: &str, value: u64| {
        Some(format!(
            "collector setting {key} = {value} must be below \
             collector.offline_drive_timeout_seconds (120)"
        ))
    };
    type Set = fn(&mut CollectorConfig, u64);
    let owner: [(&str, Set); 3] = [
        ("collector.driving_stream_poll_seconds", |c, v| {
            c.driving_stream_poll_seconds = v
        }),
        ("collector.charging_poll_seconds", |c, v| {
            c.charging_poll_seconds = v
        }),
        ("collector.online_poll_seconds", |c, v| {
            c.online_poll_seconds = v
        }),
    ];
    for mode in [CollectorMode::Legacy, CollectorMode::Provider] {
        for (key, set) in owner {
            let at = |seconds: u64| {
                with_mode(mode, tolerant(&|c: &mut CollectorConfig| set(c, seconds)))
            };
            assert_eq!(first_violation(&at(119)), None, "{key} 119 s {mode:?}");
            assert_eq!(
                first_violation(&at(120)),
                relative(key, 120),
                "{key} 120 s {mode:?}"
            );
        }
        // Milliseconds: 119_999 valid, 120_000 reported in milliseconds.
        let driving = |milliseconds: u64| {
            with_mode(
                mode,
                tolerant(&|c: &mut CollectorConfig| c.driving_poll_milliseconds = milliseconds),
            )
        };
        assert_eq!(first_violation(&driving(119_999)), None);
        assert_eq!(
            first_violation(&driving(120_000)),
            relative("collector.driving_poll_milliseconds", 120_000)
        );
        // Inactive here: the Fleet intervals at the tolerance.
        let fleet_slow = with_mode(
            mode,
            tolerant(&|c: &mut CollectorConfig| c.fleet_polling.online_poll_seconds = 120),
        );
        assert_eq!(
            first_violation(&fleet_slow),
            None,
            "inactive Fleet interval in {mode:?}"
        );
    }
    let fleet: [(&str, Set); 3] = [
        ("collector.fleet_polling.driving_poll_seconds", |c, v| {
            c.fleet_polling.driving_poll_seconds = v
        }),
        ("collector.fleet_polling.charging_poll_seconds", |c, v| {
            c.fleet_polling.charging_poll_seconds = v
        }),
        ("collector.fleet_polling.online_poll_seconds", |c, v| {
            c.fleet_polling.online_poll_seconds = v
        }),
    ];
    for (key, set) in fleet {
        let at = |seconds: u64| {
            with_mode(
                CollectorMode::FleetPolling,
                tolerant(&|c: &mut CollectorConfig| set(c, seconds)),
            )
        };
        assert_eq!(first_violation(&at(119)), None, "{key} 119 s");
        assert_eq!(first_violation(&at(120)), relative(key, 120), "{key} 120 s");
    }
    // Inactive in Fleet mode: the Owner-wire online interval at the tolerance.
    assert_eq!(
        first_violation(&with_mode(
            CollectorMode::FleetPolling,
            tolerant(&|c: &mut CollectorConfig| c.online_poll_seconds = 120),
        )),
        None
    );
    // The back-off is bound in every mode.
    for mode in ALL_MODES {
        let backoff = |seconds: u64| {
            with_mode(
                mode,
                tolerant(&|c: &mut CollectorConfig| c.max_backoff_seconds = seconds),
            )
        };
        assert_eq!(first_violation(&backoff(119)), None, "{mode:?}");
        // CMP3-01: an explicit back-off at the tolerance is moved below it, not an
        // error (the other intervals above keep theirs).
        assert_eq!(first_violation(&backoff(120)), None, "{mode:?}");
        assert_eq!(
            backoff(120).cadence().expect("clamped").maximum_backoff,
            secs(119),
            "{mode:?}"
        );
    }
}

#[test]
fn ct2_cadence_failure_order_is_stable() {
    // Two faults at a time, in the documented order: zero intervals (Fleet
    // before Owner-wire), the driving minimum, the timeout range, the charging
    // integration bound (Owner-wire before Fleet), then the relative bounds.
    type Fault = fn(&mut CollectorConfig);
    let faults: [(&str, u64, Fault); 9] = [
        ("collector.fleet_polling.driving_poll_seconds", 0, |c| {
            c.fleet_polling.driving_poll_seconds = 0
        }),
        ("collector.fleet_polling.inventory_poll_seconds", 0, |c| {
            c.fleet_polling.inventory_poll_seconds = Some(0)
        }),
        ("collector.driving_poll_milliseconds", 0, |c| {
            c.driving_poll_milliseconds = 0
        }),
        ("collector.max_backoff_seconds", 0, |c| {
            c.max_backoff_seconds = 0
        }),
        ("collector.driving_poll_milliseconds", 999, |c| {
            // Distinct from the zero fault above: only the minimum rule.
            c.driving_poll_milliseconds = 999
        }),
        ("collector.request_timeout_seconds", 121, |c| {
            c.request_timeout_seconds = 121
        }),
        ("collector.charging_poll_seconds", 600, |c| {
            c.charging_poll_seconds = 600
        }),
        ("collector.fleet_polling.charging_poll_seconds", 600, |c| {
            c.fleet_polling.charging_poll_seconds = 600
        }),
        ("collector.online_poll_seconds", 900, |c| {
            c.online_poll_seconds = 900
        }),
    ];
    let base = || CollectorConfig {
        offline_drive_timeout_seconds: 900,
        ..CollectorConfig::default()
    };
    for (index, (key, value, fault)) in faults.iter().enumerate() {
        let mut single = base();
        fault(&mut single);
        let violation = single.cadence_violation().expect("single fault");
        assert_eq!(
            (violation.key, violation.value),
            (*key, *value),
            "alone: {key}"
        );
        for (later_key, _, later) in &faults[index + 1..] {
            // A driving minimum fault and a driving zero fault are the same key.
            if later_key == key {
                continue;
            }
            let mut both = base();
            later(&mut both);
            fault(&mut both);
            let violation = both.cadence_violation().expect("two faults");
            assert_eq!(
                (violation.key, violation.value),
                (*key, *value),
                "{key} before {later_key}"
            );
        }
    }
}

#[test]
fn ct3_an_old_configuration_with_a_newly_invalid_cadence_still_loads() {
    // A file written before the back-off had to stay below the tolerance: the
    // offline timeout is 300 s and the back-off key is absent, so the default 300 s
    // applies. It loads, and for one release the back-off at the tolerance is moved
    // below it with one notice, so the file collects (B2 apply, R3C-04: the default
    // is clamped too).
    let config: HubConfig = toml::from_str(
        "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
         [collector]\noffline_drive_timeout_seconds = 300\n",
    )
    .expect("parse");
    config.validate().expect("an old configuration still loads");
    assert_eq!(config.collector.max_backoff_seconds, 300);
    assert_eq!(config.collector.cadence_violation(), None, "it collects");
    assert_eq!(
        config.collector.cadence().expect("valid").maximum_backoff,
        secs(299)
    );
    assert_eq!(
        config.collector.start_notices(),
        vec![ConfigNotice::ValueClamped {
            key: "max_backoff_seconds",
            configured: 300,
            effective: 299,
        }]
    );
    // A bound that stays an error still gives the cadence error the wait reports.
    let faulty: HubConfig = toml::from_str(
        "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
         [collector]\noffline_drive_timeout_seconds = 300\nrequest_timeout_seconds = 121\n",
    )
    .expect("parse");
    faulty.validate().expect("an old configuration still loads");
    assert_eq!(
        faulty
            .collector
            .cadence_violation()
            .map(|violation| violation.to_string()),
        Some(
            "collector setting collector.request_timeout_seconds = 121 must be between 1 \
             and 120 seconds inclusive"
                .to_owned()
        )
    );
    // With the back-off set to 240 s the first file is valid and says nothing.
    let fixed: HubConfig = toml::from_str(
        "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
         [collector]\noffline_drive_timeout_seconds = 300\nmax_backoff_seconds = 240\n",
    )
    .expect("parse");
    fixed.validate().expect("loads");
    assert_eq!(fixed.collector.cadence_violation(), None);
    assert!(fixed.collector.start_notices().is_empty());
}

#[test]
fn cadence_views_use_their_loaded_configuration_snapshot() {
    // Serve loaded a faulty file at start; the corrected file on disk is what a
    // local status or doctor loads later. The two views differ and the Serve's
    // own digest never changes, while the authentication time is read afresh at
    // every evaluation.
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let write = |name: &str, body: &str| {
        let path = temporary.path().join(name);
        fs::write(
            &path,
            format!(
                "data_dir = {:?}\nbind = '127.0.0.1:18443'\n[collector]\n{body}\n",
                temporary.path()
            ),
        )
        .expect("config");
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .expect("mode");
        path
    };
    let faulty_path = write("faulty.toml", "request_timeout_seconds = 121");
    let corrected_path = write("corrected.toml", "request_timeout_seconds = 60");
    let (faulty, faulty_digest) = HubConfig::load_with_digest(&faulty_path).expect("faulty");
    let (corrected, corrected_digest) =
        HubConfig::load_with_digest(&corrected_path).expect("corrected");
    assert_ne!(faulty_digest, corrected_digest);
    assert!(faulty.collector.cadence_violation().is_some());
    assert_eq!(corrected.collector.cadence_violation(), None);
    // Loading the corrected file did not touch the faulty snapshot or its digest.
    let (_, again) = HubConfig::load_with_digest(&faulty_path).expect("faulty again");
    assert_eq!(again, faulty_digest);
    assert!(faulty.collector.cadence_violation().is_some());

    // Both views read the effective credential status at the evaluation time they
    // are given: a usable sign-in expires between two evaluations, with no write.
    let store = crate::db::HubStore::initialize(temporary.path()).expect("store");
    let usable_until: i64 = 1_800_000_100;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let access = format!(
        "{}.{}.signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
        URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{}}}"#, usable_until + 300))
    );
    let key = b"snapshot key";
    let tokens = crate::credentials::OwnerTokens::from_secret_parts(access, "refresh".to_owned())
        .expect("tokens");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key, &tokens).expect("encrypt");
    let stored = crate::db::TeslaMateLegacyTokenStore::refreshed(
        access,
        refresh,
        usable_until + 1_000,
        usable_until - 500,
    )
    .expect("pair");
    crate::teslamate_credentials::replace_key_and_tokens(temporary.path(), &store, key, &stored)
        .expect("persist");
    let generation = store
        .load_teslamate_legacy_tokens()
        .expect("load")
        .expect("row")
        .credential_generation()
        .expect("generation");
    store
        .write_collector_auth_row(
            &crate::db::CollectorAuthRow {
                state: "sign_in_required".to_owned(),
                reason: Some("refresh_token_rejected".to_owned()),
                collecting: true,
                retry_at: None,
                stopped_generation: Some(generation),
                stopped_mode: None,
                fuse_consecutive: 0,
                fuse_generation: None,
                fuse_last_rejection_at: None,
                fuse_pause_until: None,
                fuse_rejected_access_digest: None,
                clock_skew: false,
            },
            1_800_000_000_000,
        )
        .expect("row");
    for config in [&faulty, &corrected] {
        let mode = collector_mode(&config.collector);
        let state_at = |now_ms: i64| {
            store
                .effective_collector_auth(mode, now_ms)
                .expect("effective")
                .expect("status")
                .row
                .state
        };
        assert_eq!(state_at(usable_until * 1_000 - 1), "sign_in_required");
        assert_eq!(state_at(usable_until * 1_000), "signed_out");
    }
    assert_eq!(
        store.load_collector_auth_row().unwrap().unwrap().state,
        "sign_in_required"
    );
}

#[test]
fn ct2_every_session_interval_is_checked_against_the_silence_tolerance_on_its_own() {
    // R21: a session interval at or above `T_gap` could close a drive between two
    // successful polls. With `T_gap` = 120 s every other interval below is well
    // under it, so each case isolates the one interval it changes; the energy
    // bound (`K_INT_MAX_MS`, 600 s) is never the reason.
    let tolerant = |change: &dyn Fn(&mut CollectorConfig)| {
        let mut config = CollectorConfig {
            offline_drive_timeout_seconds: 120,
            max_backoff_seconds: 100,
            fleet_polling: FleetPollingConfig {
                driving_poll_seconds: 30,
                charging_poll_seconds: 60,
                online_poll_seconds: 60,
                ..FleetPollingConfig::default()
            },
            ..CollectorConfig::default()
        };
        change(&mut config);
        config
    };
    let invalid = |config: CollectorConfig| {
        matches!(
            config.cadence(),
            Err(ConfigError::InvalidCollectorCadence(_))
        )
    };
    assert!(
        tolerant(&|_: &mut CollectorConfig| {}).cadence().is_ok(),
        "the isolating base is valid in every mode: {:?}",
        ALL_MODES.map(
            |mode| with_mode(mode, tolerant(&|_: &mut CollectorConfig| {}))
                .cadence()
                .is_ok()
        )
    );

    // Owner wire: each of the four session intervals, 119 s valid and 120 s not.
    type Change = fn(&mut CollectorConfig, u64);
    let owner_keys: [(&str, Change); 4] = [
        ("charging", |config: &mut CollectorConfig, seconds: u64| {
            config.charging_poll_seconds = seconds
        }),
        ("online", |config: &mut CollectorConfig, seconds: u64| {
            config.online_poll_seconds = seconds
        }),
        (
            "driving with a healthy stream",
            |config: &mut CollectorConfig, seconds: u64| {
                config.driving_stream_poll_seconds = seconds;
            },
        ),
        (
            "driving without one",
            |config: &mut CollectorConfig, seconds: u64| {
                config.driving_poll_milliseconds = seconds * 1_000;
            },
        ),
    ];
    for mode in [CollectorMode::Legacy, CollectorMode::Provider] {
        for (name, set) in owner_keys {
            assert!(
                !invalid(with_mode(
                    mode,
                    tolerant(&|config: &mut CollectorConfig| set(config, 119))
                )),
                "Owner-wire {name} 119 s with T_gap 120 s in {mode:?}"
            );
            assert!(
                invalid(with_mode(
                    mode,
                    tolerant(&|config: &mut CollectorConfig| set(config, 120))
                )),
                "Owner-wire {name} 120 s with T_gap 120 s in {mode:?}"
            );
        }
    }

    // Fleet polling: its three session intervals, checked only in that mode.
    let fleet_keys: [(&str, Change); 3] = [
        ("driving", |config: &mut CollectorConfig, seconds: u64| {
            config.fleet_polling.driving_poll_seconds = seconds;
        }),
        ("charging", |config: &mut CollectorConfig, seconds: u64| {
            config.fleet_polling.charging_poll_seconds = seconds;
        }),
        ("online", |config: &mut CollectorConfig, seconds: u64| {
            config.fleet_polling.online_poll_seconds = seconds;
        }),
    ];
    for (name, set) in fleet_keys {
        assert!(
            !invalid(with_mode(
                CollectorMode::FleetPolling,
                tolerant(&|config: &mut CollectorConfig| set(config, 119))
            )),
            "Fleet {name} 119 s with T_gap 120 s"
        );
        assert!(
            invalid(with_mode(
                CollectorMode::FleetPolling,
                tolerant(&|config: &mut CollectorConfig| set(config, 120))
            )),
            "Fleet {name} 120 s with T_gap 120 s"
        );
        // The Owner wire does not poll by these keys, so it does not check them.
        assert!(
            !invalid(with_mode(
                CollectorMode::Legacy,
                tolerant(&|config: &mut CollectorConfig| set(config, 120))
            )),
            "Fleet {name} 120 s is harmless on the Owner wire"
        );
    }

    // The longest back-off is a session interval of every mode too.
    for mode in ALL_MODES {
        assert!(!invalid(with_mode(
            mode,
            tolerant(&|config: &mut CollectorConfig| config.max_backoff_seconds = 119)
        )));
        // CMP3-01: moved below the tolerance, not rejected.
        assert!(!invalid(with_mode(
            mode,
            tolerant(&|config: &mut CollectorConfig| config.max_backoff_seconds = 120)
        )));
    }
}

#[test]
fn ct2_the_active_mode_decides_which_read_intervals_are_checked_against_the_silence_tolerance() {
    let slow_fleet = CollectorConfig {
        fleet_polling: FleetPollingConfig {
            online_poll_seconds: 700,
            ..FleetPollingConfig::default()
        },
        ..CollectorConfig::default()
    };
    // Only the active mode's intervals are checked: 700 s Fleet idle polling
    // is harmless while the Owner wire collects, and an error in Fleet mode.
    assert!(slow_fleet.cadence().is_ok());
    assert!(
        with_mode(CollectorMode::Provider, slow_fleet.clone())
            .cadence()
            .is_ok()
    );
    assert!(matches!(
        with_mode(CollectorMode::FleetPolling, slow_fleet).cadence(),
        Err(ConfigError::InvalidCollectorCadence(_))
    ));

    let slow_owner_wire = CollectorConfig {
        online_poll_seconds: 700,
        ..CollectorConfig::default()
    };
    assert!(matches!(
        slow_owner_wire.cadence(),
        Err(ConfigError::InvalidCollectorCadence(_))
    ));
    assert!(
        with_mode(CollectorMode::FleetPolling, slow_owner_wire)
            .cadence()
            .is_ok()
    );

    // Both Owner-wire Driving intervals are session intervals.
    let slow_stream_driving = CollectorConfig {
        driving_stream_poll_seconds: 600,
        ..CollectorConfig::default()
    };
    assert!(matches!(
        slow_stream_driving.cadence(),
        Err(ConfigError::InvalidCollectorCadence(_))
    ));
    let slow_driving = CollectorConfig {
        driving_poll_milliseconds: 600_000,
        ..CollectorConfig::default()
    };
    assert!(matches!(
        slow_driving.cadence(),
        Err(ConfigError::InvalidCollectorCadence(_))
    ));
}

#[test]
fn ct2_lower_bounds_zero_values_and_the_back_off_cap_are_rejected() {
    let base = CollectorConfig::default();
    let invalid = |config: CollectorConfig| {
        matches!(
            config.cadence(),
            Err(ConfigError::InvalidCollectorCadence(_))
        )
    };
    // Tesla's realtime limit is 60 requests a minute: nothing faster than 1 s.
    assert!(invalid(CollectorConfig {
        driving_poll_milliseconds: 999,
        ..base.clone()
    }));
    assert!(
        CollectorConfig {
            driving_poll_milliseconds: 1_000,
            ..base.clone()
        }
        .cadence()
        .is_ok()
    );
    // CMP3-01: a back-off at the silence tolerance is moved to 599 s, in every mode.
    for mode in ALL_MODES {
        let cadence = with_mode(
            mode,
            CollectorConfig {
                max_backoff_seconds: 600,
                ..base.clone()
            },
        )
        .cadence()
        .expect("clamped");
        assert_eq!(cadence.maximum_backoff, secs(599), "{mode:?}");
    }
    assert!(
        CollectorConfig {
            max_backoff_seconds: 599,
            ..base.clone()
        }
        .cadence()
        .is_ok()
    );

    // Fleet table: a zero interval is an error in every mode, a zero cap is not.
    for mode in ALL_MODES {
        for broken in [
            FleetPollingConfig {
                driving_poll_seconds: 0,
                ..FleetPollingConfig::default()
            },
            FleetPollingConfig {
                charging_poll_seconds: 0,
                ..FleetPollingConfig::default()
            },
            FleetPollingConfig {
                updating_poll_seconds: 0,
                ..FleetPollingConfig::default()
            },
            FleetPollingConfig {
                online_poll_seconds: 0,
                ..FleetPollingConfig::default()
            },
            FleetPollingConfig {
                state_poll_seconds: Some(0),
                ..FleetPollingConfig::default()
            },
            FleetPollingConfig {
                inventory_poll_seconds: Some(0),
                ..FleetPollingConfig::default()
            },
        ] {
            assert!(
                invalid(with_mode(
                    mode,
                    CollectorConfig {
                        fleet_polling: broken,
                        ..base.clone()
                    }
                )),
                "zero Fleet interval in {mode:?}"
            );
        }
        let zero_cap = with_mode(
            mode,
            CollectorConfig {
                fleet_polling: FleetPollingConfig {
                    monthly_cap_cents: 0,
                    ..FleetPollingConfig::default()
                },
                ..base.clone()
            },
        );
        assert!(
            zero_cap.cadence().is_ok(),
            "a zero cap stops billing, not loading"
        );
    }

    // Every Owner-wire interval must be above zero.
    for broken in [
        CollectorConfig {
            driving_stream_poll_seconds: 0,
            ..base.clone()
        },
        CollectorConfig {
            charging_poll_seconds: 0,
            ..base.clone()
        },
        CollectorConfig {
            online_poll_seconds: 0,
            ..base.clone()
        },
        CollectorConfig {
            sleeping_poll_seconds: 0,
            ..base.clone()
        },
        CollectorConfig {
            suspended_poll_seconds: 0,
            ..base.clone()
        },
        CollectorConfig {
            updating_poll_seconds: 0,
            ..base.clone()
        },
        CollectorConfig {
            offline_drive_timeout_seconds: 0,
            ..base.clone()
        },
        CollectorConfig {
            max_backoff_seconds: 0,
            ..base.clone()
        },
    ] {
        assert!(invalid(broken));
    }

    // A negative cap cannot be written: the key is unsigned.
    assert!(
        toml::from_str::<HubConfig>(
            "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
             [collector.fleet_polling]\nmonthly_cap_cents = -1\n"
        )
        .is_err()
    );
}

#[test]
fn ct2_request_timeout_is_between_one_and_one_hundred_and_twenty_seconds() {
    let with_timeout = |seconds| CollectorConfig {
        request_timeout_seconds: seconds,
        ..CollectorConfig::default()
    };
    assert!(with_timeout(1).cadence().is_ok());
    assert!(with_timeout(120).cadence().is_ok());
    for seconds in [0, 121] {
        assert!(matches!(
            with_timeout(seconds).cadence(),
            Err(ConfigError::InvalidCollectorCadence(_))
        ));
    }
}

#[test]
fn ct2_stream_health_timeout_is_clamped_into_sixty_to_three_hundred_seconds() {
    let with_timeout = |seconds| CollectorConfig {
        stream_health_timeout_seconds: seconds,
        ..CollectorConfig::default()
    };
    for (configured, effective) in [
        (30_u64, 60_u64),
        (400, 300),
        (0, 60),
        (60, 60),
        (300, 300),
        (150, 150),
    ] {
        let config = with_timeout(configured);
        assert_eq!(
            config.cadence().unwrap().stream_health_timeout,
            secs(effective),
            "configured {configured}"
        );
        let notices = config.start_notices();
        if configured == effective {
            assert!(notices.is_empty(), "no event for {configured}");
        } else {
            assert_eq!(
                notices,
                vec![ConfigNotice::ValueClamped {
                    key: "stream_health_timeout_seconds",
                    configured,
                    effective,
                }],
                "exactly one clamp event for {configured}"
            );
            assert_eq!(notices[0].code(), "config_value_clamped");
        }
    }
    // It loads: a clamped value is never a configuration error.
    let loaded = HubConfig::from_exact_bytes(
        b"data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
          [collector]\nstream_health_timeout_seconds = 30\n",
    )
    .expect("a short timeout still loads")
    .0;
    assert_eq!(
        loaded.collector.cadence().unwrap().stream_health_timeout,
        secs(60)
    );
}

#[test]
fn ct3_an_old_configuration_with_every_collector_key_still_loads() {
    let old = "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
        [collector]\n\
        provider = 'legacy'\n\
        owner_api_base_url = 'https://owner-api.teslamotors.com/'\n\
        request_timeout_seconds = 20\n\
        interval_seconds = 300\n\
        max_backoff_seconds = 240\n\
        driving_poll_milliseconds = 5000\n\
        charging_poll_seconds = 60\n\
        online_poll_seconds = 90\n\
        sleeping_poll_seconds = 45\n\
        offline_drive_timeout_seconds = 900\n\
        idle_suspend_after_seconds = 1200\n\
        suspended_poll_seconds = 60\n\
        updating_poll_seconds = 180\n\
        stream_endpoint_override = 'wss://stream.provider.example.test/streaming/'\n\
        stream_region = 'global'\n\
        fleet_command_proxy_url = 'https://127.0.0.1:4445/'\n\
        fleet_command_proxy_root_certificate_path = '/etc/teslatlas-hub/command-proxy-cert.pem'\n\
        stream_health_timeout_seconds = 120\n\
        [collector.legacy_auth]\nenabled = true\n";
    let (config, _digest) = HubConfig::from_exact_bytes(old.as_bytes()).expect("old config loads");
    assert_eq!(config.collector.idle_suspend_after_seconds, 1_200);
    assert_eq!(config.collector.driving_poll_milliseconds, 5_000);
    assert_eq!(
        config.collector.start_notices(),
        vec![ConfigNotice::KeyIgnored {
            key: "idle_suspend_after_seconds"
        }]
    );
    assert_eq!(
        config.collector.start_notices()[0].code(),
        "config_key_ignored"
    );
    // The new keys keep their defaults.
    let cadence = config.collector.cadence().unwrap();
    assert_eq!(cadence.driving_stream, secs(60));
    assert_eq!(cadence.fleet, FleetCadence::DEFAULT);
    // The ignored key does not reach the cadence.
    assert_eq!(cadence.maximum_backoff, secs(240));

    // The default value of the ignored key is not reported.
    assert!(CollectorConfig::default().start_notices().is_empty());

    // Unknown keys still fail, in the collector table and in the new table.
    for unknown in [
        "[collector]\nunknown_collector_key = 1\n",
        "[collector.fleet_polling]\nunknown_fleet_key = 1\n",
    ] {
        let text = format!("data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n{unknown}");
        assert!(toml::from_str::<HubConfig>(&text).is_err(), "{unknown}");
    }
}

#[test]
fn ct58_mode_follows_the_origin_of_the_owner_api_base_url() {
    let mode_of = |base: Option<&str>| {
        collector_mode(&CollectorConfig {
            owner_api_base_url: base.map(str::to_owned),
            ..CollectorConfig::default()
        })
    };
    // Tesla's global default, however it is written, stays Legacy.
    for legacy in [
        "https://owner-api.teslamotors.com/",
        "https://owner-api.teslamotors.com",
        "https://owner-api.teslamotors.com//",
        "https://OWNER-API.TESLAMOTORS.COM/",
        "https://owner-api.teslamotors.com:443/",
        "https://owner-api.teslamotors.com./",
        "https://owner-api.teslamotors.com/api/1/?ignored=true",
        // The China default is a Tesla origin too.
        "https://owner-api.vn.cloud.tesla.cn/",
        "https://Owner-Api.VN.Cloud.Tesla.CN:443",
    ] {
        assert_eq!(mode_of(Some(legacy)), CollectorMode::Legacy, "{legacy}");
    }
    // Any other origin is a provider, including another port or scheme.
    for provider in [
        PROVIDER_BASE,
        "https://api.provider.example.test/",
        "https://owner-api.teslamotors.com:8443/",
        "http://owner-api.teslamotors.com/",
        "https://owner-api.teslamotors.com.evil.example.test/",
        "https://owner-api.teslamotors.net/",
    ] {
        assert_eq!(
            mode_of(Some(provider)),
            CollectorMode::Provider,
            "{provider}"
        );
    }
    // No data host and an unparsable one.
    assert_eq!(mode_of(None), CollectorMode::Legacy);
    assert_eq!(mode_of(Some("not a url")), CollectorMode::Provider);
}

#[test]
fn ct58_token_url_never_selects_the_mode_and_fleet_polling_wins() {
    // `collector_mode` reads only `provider` and `owner_api_base_url`, so a
    // `token_url` can never select the mode. The key itself and the
    // `configuration_error(token_url_not_allowed)` that C5 raises for it are
    // C5's (C5 R7, C5 test 13) and are not in this configuration yet.
    let fleet = CollectorConfig {
        provider: CollectorProvider::Fleet,
        owner_api_base_url: Some(PROVIDER_BASE.to_owned()),
        ..CollectorConfig::default()
    };
    assert_eq!(collector_mode(&fleet), CollectorMode::FleetPolling);
    assert_eq!(
        collector_mode(&CollectorConfig {
            provider: CollectorProvider::Fleet,
            ..CollectorConfig::default()
        }),
        CollectorMode::FleetPolling
    );
    assert_eq!(CollectorMode::Legacy.code(), "legacy");
    assert_eq!(CollectorMode::Provider.code(), "provider");
    assert_eq!(CollectorMode::FleetPolling.code(), "fleet_polling");
    assert!(CollectorMode::Legacy.is_owner_wire());
    assert!(CollectorMode::Provider.is_owner_wire());
    assert!(!CollectorMode::FleetPolling.is_owner_wire());
}

// ---- B2 round 3, CMP3-01 (coordinator decision): the old back-off is clamped ------

#[test]
fn cmp3_01_the_last_releases_guide_example_loads_logs_one_notice_and_collects() {
    // The previous release's own default (900 s) and its configuration guide's example
    // set `max_backoff_seconds` above the offline timeout (600 s) that this release
    // enforces. For one release an explicit value at or above the timeout is moved to
    // the largest valid one, 599 s, with one `config_value_clamped` notice, so the
    // upgrade collects instead of waiting in the cadence wait (G2-10).
    let old = "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n\
        [collector]\n\
        provider = 'legacy'\n\
        interval_seconds = 60\n\
        max_backoff_seconds = 900\n\
        driving_poll_milliseconds = 2500\n\
        stream_health_timeout_seconds = 30\n";
    let (config, _digest) =
        HubConfig::from_exact_bytes(old.as_bytes()).expect("the old guide example loads");
    assert_eq!(config.collector.cadence_violation(), None, "it collects");
    let cadence = config.collector.cadence().expect("a valid cadence");
    assert_eq!(cadence.maximum_backoff, secs(599));
    assert_eq!(cadence.driving, Duration::from_millis(2_500));
    let notices = config.collector.start_notices();
    let backoff: Vec<&ConfigNotice> = notices
        .iter()
        .filter(|notice| {
            matches!(notice, ConfigNotice::ValueClamped { key, .. } if *key == "max_backoff_seconds")
        })
        .collect();
    assert_eq!(
        backoff,
        [&ConfigNotice::ValueClamped {
            key: "max_backoff_seconds",
            configured: 900,
            effective: 599,
        }],
        "exactly one clamp notice for the back-off: {notices:?}"
    );
    assert_eq!(backoff[0].code(), "config_value_clamped");
    // The other key of the example that left its range keeps its own notice.
    assert!(notices.contains(&ConfigNotice::ValueClamped {
        key: "stream_health_timeout_seconds",
        configured: 30,
        effective: 60,
    }));

    // The clamp follows the timeout: the largest valid value below it, in every mode.
    for mode in ALL_MODES {
        let config = with_mode(
            mode,
            CollectorConfig {
                offline_drive_timeout_seconds: 400,
                max_backoff_seconds: 500,
                ..CollectorConfig::default()
            },
        );
        assert_eq!(
            config.cadence().expect("clamped").maximum_backoff,
            secs(399),
            "{mode:?}"
        );
        assert_eq!(
            config.start_notices(),
            vec![ConfigNotice::ValueClamped {
                key: "max_backoff_seconds",
                configured: 500,
                effective: 399,
            }],
            "{mode:?}"
        );
    }
    // A value below the timeout is not touched and says nothing.
    assert!(CollectorConfig::default().start_notices().is_empty());
    assert_eq!(
        CollectorConfig {
            max_backoff_seconds: 599,
            ..CollectorConfig::default()
        }
        .start_notices(),
        Vec::new()
    );

    // Every other newly enforced bound stays an error.
    let invalid = |config: CollectorConfig| {
        matches!(
            config.cadence(),
            Err(ConfigError::InvalidCollectorCadence(_))
        )
    };
    let base = CollectorConfig::default();
    assert!(invalid(CollectorConfig {
        driving_poll_milliseconds: 999,
        ..base.clone()
    }));
    assert!(invalid(CollectorConfig {
        request_timeout_seconds: 121,
        ..base.clone()
    }));
    assert!(invalid(CollectorConfig {
        charging_poll_seconds: 600,
        ..base.clone()
    }));
    assert!(invalid(CollectorConfig {
        online_poll_seconds: 600,
        ..base.clone()
    }));
    // The default back-off (300 s) with a timeout of 300 s is moved too (B2 apply,
    // R3C-04, CT3): the file that only lowered the timeout collects, with one notice.
    for mode in ALL_MODES {
        let lowered = with_mode(
            mode,
            CollectorConfig {
                offline_drive_timeout_seconds: 300,
                ..base.clone()
            },
        );
        assert_eq!(
            lowered.cadence().expect("clamped").maximum_backoff,
            secs(299),
            "{mode:?}"
        );
        assert_eq!(
            lowered.start_notices(),
            vec![ConfigNotice::ValueClamped {
                key: "max_backoff_seconds",
                configured: 300,
                effective: 299,
            }],
            "{mode:?}"
        );
    }
    // A timeout of one second leaves no valid back-off below it.
    assert!(invalid(CollectorConfig {
        offline_drive_timeout_seconds: 1,
        max_backoff_seconds: 5,
        ..base.clone()
    }));
}
