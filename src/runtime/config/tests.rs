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
    collector.stream_endpoint_override = Some("wss://stream.example/?refresh-secret=1".to_owned());
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
