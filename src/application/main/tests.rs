// SPDX-License-Identifier: AGPL-3.0-only

use std::path::PathBuf;
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    time::Duration,
};
#[cfg(target_os = "macos")]
use std::{
    future::pending,
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    process::{Child, Command as ProcessCommand, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use rcgen::{CertifiedKey, generate_simple_self_signed};
use sha2::Digest;
#[cfg(target_os = "macos")]
use sha2::Sha256;

use super::{
    Cli, Command, ControlCommand, CurrentVehicleStatusFields, MAX_TLS_CERTIFICATE_CHAIN_BYTES,
    MAX_TLS_PRIVATE_KEY_BYTES, PairingCommandError, PairingCommandInput,
    build_pairing_presentation, current_vehicle_status_fields, execute_pairing_at,
    leaf_certificate_sha256, leaf_certificate_sha256_after_open, pairing_uri,
    persist_and_present_pairing, read_tls_identity_file, run,
    run_immutable_diagnostic_with,
};
#[cfg(target_os = "macos")]
use super::{
    DevelopmentServeModeArgument, HubConfig, MAC_COMMAND_PROXY_RETRY_DELAY,
    MAX_MIGRATION_ENCRYPTION_KEY_BYTES, MAX_MIGRATION_POSTGRES_PASSWORD_BYTES,
    MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES, MAX_MIGRATION_TOKEN_BYTES,
    MAX_MIGRATION_TOKEN_FILE_BYTES, MacCommandProxySpec, MacServeControl,
    MacServeWorkerStopTimeout, MigrationSecretReadError, clear_provider_credentials,
    migration_start_requested,
    migration_stop_confirmed,
    read_migration_encryption_key, read_migration_postgres_password, read_migration_secret,
    read_migration_secret_file_with_hooks, run_macos_serve_supervisor,
    should_emit_migration_progress,
    source_check_failure_details, source_db_version_confirmation, validate_history_only_config,
    validate_streaming_setting, write_migration_progress_event,
};
use tesatlas_hub::db::{HubStore, ObservationInput, SourceDescriptor, VehicleDescriptor};
#[cfg(target_os = "macos")]
use tesatlas_hub::protocol::{
    CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V3, OpaqueCursor, PROTOCOL_V1,
};
#[cfg(target_os = "macos")]
use tesatlas_hub::source_db_progress::{SourceDbMigrationPhase, SourceDbMigrationProgressEvent};
#[cfg(target_os = "macos")]
use tesatlas_hub::{
    source_db_reader::SourceDbReaderError, source_db_schema::SchemaCompatibilityError,
};
use uuid::Uuid;

#[path = "b3_c6_s2_tests.rs"]
mod b3_c6_s2;
#[path = "b3_c8_k_tests.rs"]
mod b3_c8_k;
#[path = "charge_cost_cli_tests.rs"]
mod charge_cost_cli_tests;
#[path = "collector_cli_tests.rs"]
mod collector_cli_tests;
#[cfg(target_os = "macos")]
#[path = "fleet_cli_tests.rs"]
mod fleet_cli_tests;
#[path = "pairing_scope_tests.rs"]
mod pairing_scope_tests;
#[path = "readiness_cli_tests.rs"]
mod readiness_cli_tests;

#[test]
fn status_vehicle_fields_preserve_current_projection_presence_and_absence() {
    let temporary = tempfile::tempdir().expect("temporary Hub");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
        .expect("private data directory");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("tesla_owner_api", "status-test"),
            1_000,
        )
        .expect("source");
    let observed = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "observed"), 1_001)
        .expect("observed vehicle");
    let missing = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "missing"), 1_002)
        .expect("missing vehicle");
    let offline = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "offline"), 1_003)
        .expect("offline vehicle");
    let asleep = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "asleep"), 1_004)
        .expect("asleep vehicle");
    store
        .upsert_geofences(
            observed.vehicle_id,
            &[tesatlas_hub::source_db_projection::SourceDbGeofence {
                id: 1,
                name: "Home".into(),
                latitude: Some(51.0),
                longitude: Some(-0.1),
                radius_m: Some(150.0),
                billing_type: Some(tesatlas_hub::hub_pack::GeofenceBillingType::PerKwh),
                cost_per_unit: None,
                session_fee: None,
            }],
        )
        .expect("geofence");
    store
        .accept_owner_observation_and_lifecycle(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: observed.vehicle_id,
                observed_at_ms: 2_000,
                payload: serde_json::json!({
                    "record_type": "owner_api_vehicle_data_v1",
                    "source_vehicle_state": "online",
                    "vehicle_data": {
                        "drive_state": {"latitude": 51.0, "longitude": -0.1},
                        "charge_state": {"battery_level": 81}
                    }
                }),
            },
            2_001,
            1,
        )
        .expect("current observation");
    for (vehicle_id, state, observed_at_ms) in [
        (offline.vehicle_id, "offline", 10),
        (asleep.vehicle_id, "asleep", 20),
    ] {
        store
            .append_observation(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id,
                    observed_at_ms,
                    payload: serde_json::json!({
                        "record_type": "owner_api_discovery_v1",
                        "source_vehicle_state": state,
                    }),
                },
                observed_at_ms + 1,
            )
            .expect("discovery observation");
    }

    let read_only = HubStore::open_read_only(temporary.path()).expect("read-only store");
    assert_eq!(
        current_vehicle_status_fields(&read_only, observed.vehicle_id, 2_001, 600_000)
            .expect("observed status"),
        CurrentVehicleStatusFields {
            activity_state: Some("online".into()),
            battery_level: Some(81),
            location_name: None,
            telemetry_status: "cached",
        }
    );
    assert_eq!(
        current_vehicle_status_fields(&read_only, missing.vehicle_id, 2_001, 600_000)
            .expect("missing status"),
        CurrentVehicleStatusFields {
            activity_state: None,
            battery_level: None,
            location_name: None,
            telemetry_status: "unknown",
        }
    );
    for (vehicle_id, state) in [
        (offline.vehicle_id, "offline"),
        (asleep.vehicle_id, "asleep"),
    ] {
        assert_eq!(
            current_vehicle_status_fields(&read_only, vehicle_id, 2_001, 600_000)
                .expect("discovery status"),
            CurrentVehicleStatusFields {
                activity_state: Some(state.into()),
                battery_level: None,
                location_name: None,
                telemetry_status: "cached",
            }
        );
    }
}

#[test]
fn legal_aliases_and_source_command_parse_without_configuration() {
    for name in ["legal", "licence", "license"] {
        let cli = Cli::try_parse_from(["tesatlas-hub", name]).expect("legal CLI alias");
        assert!(matches!(cli.command, Command::Legal));
    }
    let source = Cli::try_parse_from(["tesatlas-hub", "source"]).expect("source CLI");
    assert!(matches!(source.command, Command::Source));

    let healthcheck = Cli::try_parse_from([
        "tesatlas-hub",
        "healthcheck",
        "--ca-file",
        "/etc/tesatlas-hub/tls/ca.pem",
        "--server-name",
        "hub.example.invalid",
    ])
    .expect("healthcheck CLI");
    assert!(matches!(
        healthcheck.command,
        Command::Healthcheck {
            ca_file,
            server_name,
        }
            if ca_file == *"/etc/tesatlas-hub/tls/ca.pem"
                && server_name == "hub.example.invalid"
    ));
}

#[cfg(unix)]
#[test]
fn container_healthcheck_does_not_open_hub_state() {
    assert!(!tesatlas_hub::runtime::operator::admission::requires_user_hub_admission(
        &Command::Healthcheck {
            ca_file: PathBuf::from("/etc/tesatlas-hub/tls/ca.pem"),
            server_name: "hub.example.invalid".to_owned(),
        }
    ));
}

#[cfg(target_os = "macos")]
fn supervisor_cursor_proof(cursor_key: &CursorKey) -> String {
    OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V3,
            installation_id: Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            account_id: Uuid::from_u128(0x22222222_2222_4222_8222_222222222222),
            vehicle_id: Uuid::from_u128(0x33333333_3333_4333_8333_333333333333),
            generation: 7,
            sequence: 11,
        },
    )
    .expect("test cursor")
    .as_str()
    .to_owned()
}

#[cfg(target_os = "macos")]
#[test]
fn macos_command_proxy_arguments_use_private_data_paths() {
    let spec = MacCommandProxySpec {
        executable: PathBuf::from(
            "/Library/Application Support/Tesatlas Hub/bin/tesla-http-proxy",
        ),
        host: "127.0.0.1".to_owned(),
        port: 4443,
        command_key: PathBuf::from("/private/data/secrets/fleet-command-key.pem"),
        certificate: PathBuf::from("/private/data/fleet-proxy-tls-cert.pem"),
        tls_key: PathBuf::from("/private/data/secrets/fleet-proxy-tls-key.pem"),
        session_cache: PathBuf::from("/private/data/fleet-command-session-cache.json"),
    };
    assert_eq!(
        spec.arguments(),
        vec![
            "-host",
            "127.0.0.1",
            "-port",
            "4443",
            "-key-file",
            "/private/data/secrets/fleet-command-key.pem",
            "-cert",
            "/private/data/fleet-proxy-tls-cert.pem",
            "-tls-key",
            "/private/data/secrets/fleet-proxy-tls-key.pem",
            "-session-cache",
            "/private/data/fleet-command-session-cache.json",
        ]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_command_proxy_readiness_retry_is_cpu_bounded() {
    assert!(MAC_COMMAND_PROXY_RETRY_DELAY >= Duration::from_millis(10));
    assert!(MAC_COMMAND_PROXY_RETRY_DELAY <= Duration::from_millis(250));
}

#[cfg(target_os = "macos")]
#[test]
fn migration_stop_and_start_prompts_are_independent_and_default_to_no() {
    assert!(!migration_stop_confirmed(""));
    assert!(!migration_stop_confirmed("n"));
    assert!(migration_stop_confirmed(" Y\n"));
    assert!(!migration_start_requested(""));
    assert!(!migration_start_requested("N"));
    assert!(migration_start_requested("y"));
}

#[cfg(target_os = "macos")]
#[test]
fn migration_progress_is_online_snapshot_only() {
    assert!(should_emit_migration_progress(true));
    assert!(!should_emit_migration_progress(false));
}

/// Finding 6 (M02-T02 round 3): the dispatcher itself refuses the raw-token
/// forms of `setup` and `setup-fleet` first. The configuration path is absent
/// and the token file sits in a sealed directory, so any configuration read,
/// admission, database initialisation or token read would surface a different
/// error, and the data directory would come into existence.
#[cfg(unix)]
#[tokio::test]
async fn source_credential_disabled_raw_token_setup_is_refused_by_the_dispatcher_first() {
    // No token input exists any more (M02-T10): Fleet telemetry is refused before the configuration is
    // read; `setup` and `setup-fleet` are forwarded to the service, so they read the configuration only.
    let temporary = tempfile::tempdir().expect("temporary directory");
    let sealed = temporary.path().join("sealed");
    fs::create_dir(&sealed).expect("sealed directory");
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).expect("seal");
    let config_path = temporary.path().join("absent-config.toml");
    let error = run(Cli { config: Some(config_path.clone()), command: Command::ConfigureFleetTelemetry }).await
        .expect_err("Fleet telemetry is refused").to_string();
    assert_eq!(error, "provider_parked: Fleet telemetry push is not offered");
    for command in [
        Command::Setup { vehicle_id: Some(7), all_vehicles: false },
        Command::SetupFleet { step: None, vehicle_id: None, all_vehicles: true },
        Command::SetupFleet { step: Some(tesatlas_hub::runtime::operator::cli::FleetSetupStep::Begin), vehicle_id: None, all_vehicles: false },
    ] {
        let error = run(Cli { config: Some(config_path.clone()), command }).await
            .expect_err("setup without a configuration");
        assert!(!error.to_string().starts_with("source_credential_disabled"), "{error}");
    }
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o700)).expect("unseal");
    let entries: Vec<_> = fs::read_dir(temporary.path()).expect("listing").map(|entry| entry.expect("entry").file_name()).collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("sealed")], "nothing was created or read");
}

/// Source credentials are disabled: every `migrate` key or token input and
/// every `setup` token input is refused with the one stable code before any
/// named path is opened, the configuration is read or a store is created.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn source_credential_disabled_cli_inputs_are_refused_before_any_read() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let sealed = temporary.path().join("sealed");
    fs::create_dir(&sealed).expect("sealed directory");
    let secret = sealed.join("unread");
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).expect("seal");
    let data_dir = temporary.path().join("data");
    fs::create_dir(&data_dir).expect("data directory");
    fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700)).expect("private data directory");
    let admission = super::AdmittedUserHub::admit(&data_dir).expect("admission");
    let config_path = temporary.path().join("absent-config.toml");
    for (key, access, refresh) in [(Some(&secret), None, None), (None, Some(&secret), Some(&secret)), (None, Some(&secret), None)] {
        let error = super::run_macos_migration(&admission, super::MacMigrationInput {
            config_path: &config_path,
            source_url: "postgresql://reader@127.0.0.1:9/sourcedb",
            car_id: 42,
            source_key: None,
            new_import_source: false,
            postgres_password_file: &secret,
            history_only: false,
            encryption_key_file: key.map(PathBuf::as_path),
            access_token_file: access.map(PathBuf::as_path),
            refresh_token_file: refresh.map(PathBuf::as_path),
            online_snapshot: true,
            preserve_existing_credentials: false,
        }).await.expect_err("source credential input").to_string();
        assert!(error.starts_with("source_credential_disabled: "), "{error}");
    }
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o700)).expect("unseal");
    assert!(!config_path.exists() && !data_dir.join("hub.sqlite").exists(), "nothing was created");
}

#[cfg(target_os = "macos")]
#[test]
fn migration_progress_writer_emits_and_flushes_one_ndjson_line() {
    #[derive(Default)]
    struct FlushProbe {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for FlushProbe {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    let event = SourceDbMigrationProgressEvent::new(SourceDbMigrationPhase::Positions, 123, 456);
    let mut output = FlushProbe::default();
    write_migration_progress_event(&mut output, &event).expect("write progress");

    assert_eq!(
        output.bytes,
        br#"{"event":"migration_progress","completedRows":123,"totalRows":456,"phase":"positions"}
"#
    );
    assert_eq!(output.flushes, 1);
}

#[cfg(target_os = "macos")]
#[test]
fn source_check_failures_are_redacted_and_actionable() {
    let older = SourceDbReaderError::Schema(SchemaCompatibilityError::LegacyMigration {
        found: 1,
        minimum: 2,
    });
    let newer = SourceDbReaderError::Schema(SchemaCompatibilityError::UnreviewedMigration {
        found: 3,
        maximum: 2,
    });
    let selected = SourceDbReaderError::SelectedCarMissing { selected_car_id: 7 };
    let ambiguous = SourceDbReaderError::AmbiguousOpenSession {
        drives: 2,
        charges: 1,
        states: 1,
    };

    assert_eq!(
        source_check_failure_details(&older).1,
        "older_than_v4_5_compatible_schema"
    );
    assert_eq!(source_check_failure_details(&older).2, Some(1));
    assert!(source_check_failure_details(&older).3.contains("update"));
    assert!(source_check_failure_details(&older).3.contains("4.5.0"));
    assert_eq!(
        source_check_failure_details(&newer).1,
        "newer_than_v4_5_compatible_schema"
    );
    assert!(
        source_check_failure_details(&newer)
            .3
            .contains("Do not downgrade")
    );
    assert_eq!(
        source_check_failure_details(&selected).1,
        "selected_car_missing"
    );
    assert!(!source_check_failure_details(&selected).3.contains('7'));
    assert_eq!(
        source_check_failure_details(&ambiguous).1,
        "ambiguous_open_session"
    );
    assert!(!source_check_failure_details(&ambiguous).3.contains("2"));
}

#[cfg(target_os = "macos")]
#[test]
fn source_check_reports_an_exceeded_read_budget_plainly() {
    use tesatlas_hub::source_db_reader::SourceBudgetLimit;

    for limit in [
        SourceBudgetLimit::Rows,
        SourceBudgetLimit::RowBytes,
        SourceBudgetLimit::PageBytes,
    ] {
        let exceeded = SourceDbReaderError::SourceReadBudgetExceeded {
            table: "positions",
            limit,
            maximum: 67_108_864,
        };
        let (status, reason, observed, guidance) = source_check_failure_details(&exceeded);
        assert_eq!(status, "incompatible");
        assert_eq!(reason, "source_read_budget_exceeded");
        assert_eq!(observed, None);
        // Not the connection-troubleshooting advice of an unknown error, and
        // no table name or limit value.
        assert!(guidance.contains("more rows or bytes"));
        // Independence rule (a): a new Hub GUI string names no source project.
        assert!(
            !guidance
                .to_ascii_lowercase()
                .contains(["tesl", "amate"].concat().as_str())
        );
        assert!(!guidance.contains("password-free"));
        assert!(!guidance.contains("positions"));
        assert!(!guidance.contains("67108864"));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn source_check_requires_honest_version_confirmation() {
    let unconfirmed = source_db_version_confirmation(false);
    assert_eq!(unconfirmed.0, "confirmation_required");
    assert_eq!(unconfirmed.1, "v4_5_version_unconfirmed");
    assert!(unconfirmed.2.contains("cannot establish"));

    let confirmed = source_db_version_confirmation(true);
    assert_eq!(confirmed.0, "compatible");
    assert_eq!(confirmed.1, "v4_5_compatible_schema");
    assert!(!confirmed.1.contains("exact"));
}

#[cfg(target_os = "macos")]
#[test]
fn onboarding_migration_cli_is_explicit_and_noninteractive() {
    let check = Cli::try_parse_from([
        "tesatlas-hub",
        "source-check",
        "--source",
        "postgresql://reader@localhost/sourcedb",
        "--car-id",
        "7",
        "--postgres-password-file",
        "password",
        "--accept-schema-evidence",
    ])
    .expect("compatibility-check CLI");
    assert!(matches!(
        check.command,
        Command::SourceCheck { car_id: 7, .. }
    ));
    assert!(!tesatlas_hub::runtime::operator::admission::requires_user_hub_admission(&check.command));

    assert!(
        Cli::try_parse_from([
            "tesatlas-hub",
            "migrate",
            "--source",
            "postgresql://reader@localhost/sourcedb",
            "--car-id",
            "7",
            "--postgres-password-file",
            "password",
            "--encryption-key-file",
            "key",
            "--online-snapshot",
        ])
        .is_err()
    );

    let migration = Cli::try_parse_from([
        "tesatlas-hub",
        "migrate",
        "--source",
        "postgresql://reader@localhost/sourcedb",
        "--car-id",
        "7",
        "--postgres-password-file",
        "password",
        "--online-snapshot",
        "--accept-schema-evidence",
    ])
    .expect("online migration CLI");
    assert!(matches!(
        migration.command,
        Command::Migrate {
            online_snapshot: true,
            accept_schema_evidence: true,
            ..
        }
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn history_only_cli_requires_online_capture_and_excludes_tesla_credentials() {
    let base = [
        "tesatlas-hub",
        "migrate",
        "--source",
        "postgresql://reader@localhost/sourcedb",
        "--car-id",
        "1",
        "--postgres-password-file",
        "password",
        "--accept-schema-evidence",
    ];
    let mut omitted_online = base.to_vec();
    omitted_online.push("--history-only");
    assert!(Cli::try_parse_from(omitted_online).is_err());
    let mut accepted = base.to_vec();
    accepted.extend(["--history-only", "--online-snapshot"]);
    assert!(matches!(
        Cli::try_parse_from(accepted.clone()).unwrap().command,
        Command::Migrate {
            history_only: true,
            online_snapshot: true,
            ..
        }
    ));
    for conflicting in [
        "--encryption-key-file",
        "--access-token-file",
        "--refresh-token-file",
    ] {
        let mut rejected = accepted.clone();
        rejected.extend([conflicting, "secret"]);
        assert!(Cli::try_parse_from(rejected).is_err());
    }
    let mut rejected = accepted;
    rejected.push("--preserve-existing-credentials");
    assert!(Cli::try_parse_from(rejected).is_err());
    // Source checks never inspect source credentials, so the flag that chose
    // that is gone (M02-T10).
    assert!(
        Cli::try_parse_from([
            "tesatlas-hub",
            "source-check",
            "--source",
            "postgresql://reader@localhost/sourcedb",
            "--car-id",
            "1",
            "--postgres-password-file",
            "password",
            "--history-only"
        ])
        .is_err()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn history_only_config_requires_collection_disabled() {
    let parse = |collector: &str| {
        HubConfig::from_exact_bytes(format!("data_dir = '/tmp/hub-history-only-test'\nbind = '127.0.0.1:21445'\n[collector]\n{collector}\n[collector.legacy_auth]\nenabled = false\n").as_bytes()).unwrap().0
    };
    assert!(validate_history_only_config(&parse("interval_seconds = 0")).is_ok());
    assert!(validate_history_only_config(&parse("interval_seconds = 60")).is_err());
    let mut legacy_auth = parse("interval_seconds = 0");
    legacy_auth.collector.legacy_auth.enabled = true;
    assert!(validate_history_only_config(&legacy_auth).is_err());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn source_check_invalid_source_does_not_create_hub_target() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = temporary.path().join("absent/config.toml");
    let cli = Cli::try_parse_from([
        "tesatlas-hub",
        "--config",
        config.to_str().expect("UTF-8 config path"),
        "source-check",
        "--source",
        "not-a-postgres-url",
        "--car-id",
        "7",
        "--postgres-password-file",
        "unused",
    ])
    .expect("compatibility-check CLI");

    let error = run(cli).await.expect_err("invalid source must fail");
    assert!(error.to_string().contains("see JSON report"));
    assert!(!config.exists());
    assert!(!config.parent().expect("config parent").exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn migration_without_version_confirmation_does_not_create_hub_target() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = temporary.path().join("absent/config.toml");
    let cli = Cli {
        config: Some(config.clone()),
        command: Command::Migrate {
            source_key: None,
            new_import_source: false,
            source: "not-a-postgres-url".to_owned(),
            car_id: 7,
            postgres_password_file: PathBuf::from("unused-password"),
            history_only: false,
            online_snapshot: true,
            accept_schema_evidence: false,
        },
    };

    let error = run(cli)
        .await
        .expect_err("missing version confirmation must fail before migration");
    assert!(
        error
            .to_string()
            .contains("requires --accept-schema-evidence")
    );
    assert!(!config.exists());
    assert!(!config.parent().expect("config parent").exists());
}

#[cfg(unix)]
#[test]
fn bootstrap_and_explicit_vehicle_commands_parse() {
    let bootstrap = Cli::try_parse_from(["tesatlas-hub", "bootstrap"]).expect("bootstrap CLI");
    assert!(matches!(bootstrap.command, Command::Bootstrap));
    assert!(matches!(Cli::try_parse_from(["tesatlas-hub", "control", "wake"]).expect("parsed, then refused (M09-T19)").command, Command::Control { command: ControlCommand::Wake { confirm: false }, .. }));
    let wake = Cli::try_parse_from([
        "tesatlas-hub",
        "control",
        "--vehicle-id",
        "00000000-0000-0000-0000-000000000001",
        "wake",
        "--confirm",
    ])
    .expect("confirmed wake CLI");
    assert!(matches!(
        wake.command,
        Command::Control {
            vehicle_id: Some(_),
            command: ControlCommand::Wake { confirm: true }
        }
    ));
    assert!(
        Cli::try_parse_from(["tesatlas-hub", "control", "climate-start", "--confirm"]).is_ok()
    );
}

#[test]
fn paired_device_controls_parse_without_vehicle_selection() {
    let list = Cli::try_parse_from(["tesatlas-hub", "control", "paired-devices"])
        .expect("paired-device list CLI");
    assert!(matches!(
        list.command,
        Command::Control {
            command: ControlCommand::PairedDevices,
            ..
        }
    ));
    let revoke = Cli::try_parse_from([
        "tesatlas-hub",
        "control",
        "revoke-device",
        "00000000-0000-0000-0000-000000000001",
    ])
    .expect("paired-device revoke CLI");
    assert!(matches!(
        revoke.command,
        Command::Control {
            command: ControlCommand::RevokeDevice { .. },
            ..
        }
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn migration_secret_reader_accepts_each_exact_cap_and_rejects_next_byte() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    for (name, maximum) in [
        ("postgres", MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES),
        ("token", MAX_MIGRATION_TOKEN_FILE_BYTES),
        ("key", MAX_MIGRATION_ENCRYPTION_KEY_BYTES),
    ] {
        let path = temporary.path().join(name);
        fs::write(&path, vec![b'x'; maximum]).expect("exact secret");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("safe secret mode");
        assert_eq!(
            read_migration_secret(&path, maximum)
                .expect("exact cap")
                .len(),
            maximum
        );
        fs::write(&path, vec![b'x'; maximum + 1]).expect("oversized secret");
        assert!(read_migration_secret(&path, maximum).is_err());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn migration_encryption_key_reader_accepts_normal_line_endings() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let path = temporary.path().join("encryption-key");
    for ending in [b"".as_slice(), b"\n".as_slice(), b"\r\n".as_slice()] {
        let mut value = b"source-token-key".to_vec();
        value.extend_from_slice(ending);
        fs::write(&path, value).expect("key file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("key mode");
        assert_eq!(
            read_migration_encryption_key(&path)
                .expect("line ending is not part of the key")
                .as_slice(),
            b"source-token-key"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn migration_password_and_token_semantic_boundaries_include_line_endings() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let password_path = temporary.path().join("password");
    for ending in [b"".as_slice(), b"\n".as_slice(), b"\r\n".as_slice()] {
        let mut value = vec![b'p'; MAX_MIGRATION_POSTGRES_PASSWORD_BYTES];
        value.extend_from_slice(ending);
        fs::write(&password_path, value).expect("password file");
        fs::set_permissions(&password_path, fs::Permissions::from_mode(0o600))
            .expect("password mode");
        assert_eq!(
            read_migration_postgres_password(&password_path)
                .expect("semantic password cap")
                .as_str()
                .len(),
            MAX_MIGRATION_POSTGRES_PASSWORD_BYTES
        );
    }
    fs::write(
        &password_path,
        vec![b'p'; MAX_MIGRATION_POSTGRES_PASSWORD_BYTES + 1],
    )
    .expect("oversized password");
    fs::set_permissions(&password_path, fs::Permissions::from_mode(0o600)).expect("password mode");
    assert!(read_migration_postgres_password(&password_path).is_err());

    assert_eq!(MAX_MIGRATION_TOKEN_BYTES, 16 * 1024);
    let access_path = temporary.path().join("access");
    let refresh_path = temporary.path().join("refresh");
    fs::write(
        &access_path,
        [vec![b'a'; MAX_MIGRATION_TOKEN_BYTES], b"\n".to_vec()].concat(),
    )
    .expect("access token");
    fs::write(
        &refresh_path,
        [vec![b'b'; MAX_MIGRATION_TOKEN_BYTES], b"\r\n".to_vec()].concat(),
    )
    .expect("refresh token");
    for path in [&access_path, &refresh_path] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("token mode");
    }
    // The bounded reader still reads within its cap; no command hands token
    // bytes to it any more (source credentials are disabled).
    for path in [&access_path, &refresh_path] {
        assert!(read_migration_secret(path, MAX_MIGRATION_TOKEN_FILE_BYTES).is_ok());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn migration_secret_files_require_private_nofollow_stable_descriptors() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let path = temporary.path().join("secret");
    fs::write(&path, b"migration-secret").expect("secret");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secret mode");
    assert_eq!(
        read_migration_secret(&path, 64)
            .expect("safe secret")
            .as_slice(),
        b"migration-secret"
    );

    let outside = temporary.path().join("outside");
    fs::write(&outside, b"outside-secret").expect("outside secret");
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).expect("outside mode");
    let linked = temporary.path().join("linked");
    symlink(&outside, &linked).expect("secret symlink");
    assert!(matches!(
        read_migration_secret(&linked, 64),
        Err(error) if error.downcast_ref::<MigrationSecretReadError>() == Some(&MigrationSecretReadError::UnsafeFile)
    ));

    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("unsafe mode");
    assert!(matches!(
        read_migration_secret(&path, 64),
        Err(error) if error.downcast_ref::<MigrationSecretReadError>() == Some(&MigrationSecretReadError::UnsafeFile)
    ));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("safe mode");

    let replacement = temporary.path().join("replacement");
    fs::write(&replacement, b"replacement-secret").expect("replacement");
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).expect("replacement mode");
    assert_eq!(
        read_migration_secret_file_with_hooks(
            &path,
            64,
            || { fs::rename(&replacement, &path).expect("replace secret") },
            || {}
        )
        .expect_err("replacement race"),
        MigrationSecretReadError::IdentityChanged
    );

    fs::write(&path, b"stable-secret").expect("restore secret");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore mode");
    assert_eq!(
        read_migration_secret_file_with_hooks(
            &path,
            64,
            || {},
            || { fs::write(&path, b"same-inode-secret-mutated").expect("mutate secret") }
        )
        .expect_err("same inode mutation"),
        MigrationSecretReadError::IdentityChanged
    );

    let error = read_migration_secret(&linked, 64).expect_err("unsafe link");
    let rendered = format!("{error:?}");
    assert!(!rendered.contains("outside-secret"));
    assert!(!rendered.contains(&linked.display().to_string()));
}

#[cfg(target_os = "macos")]
#[test]
fn migration_secret_reader_rejects_a_fifo_without_waiting_for_a_writer() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let path = temporary.path().join("secret.fifo");
    assert!(
        ProcessCommand::new("mkfifo")
            .arg(&path)
            .status()
            .expect("run mkfifo")
            .success()
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("FIFO mode");

    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        sender
            .send(matches!(
                read_migration_secret(&path, 64),
                Err(error)
                    if error.downcast_ref::<MigrationSecretReadError>()
                        == Some(&MigrationSecretReadError::UnsafeFile)
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

#[cfg(target_os = "macos")]
struct MacServeDropWitness {
    label: &'static str,
    drops: tokio::sync::mpsc::UnboundedSender<&'static str>,
}

#[cfg(target_os = "macos")]
impl Drop for MacServeDropWitness {
    fn drop(&mut self) {
        let _ = self.drops.send(self.label);
    }
}

#[cfg(target_os = "macos")]
const MACOS_SUPERVISOR_SIGTERM_CHILD_ENV: &str =
    "TESATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_CHILD";
#[cfg(target_os = "macos")]
const MACOS_SUPERVISOR_SIGTERM_CHILD_BIND_ENV: &str =
    "TESATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_BIND";
#[cfg(target_os = "macos")]
const MACOS_SUPERVISOR_SIGTERM_CHILD_RECEIPT_ENV: &str =
    "TESATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_RECEIPT";
#[cfg(target_os = "macos")]
const MACOS_SUPERVISOR_SIGTERM_CHILD_FIXTURE_ENV: &str =
    "TESATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_FIXTURE";
#[cfg(target_os = "macos")]
const MACOS_SUPERVISOR_SIGTERM_CHILD_RUN_ENV: &str =
    "TESATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_RUN";
#[cfg(target_os = "macos")]
const MACOS_SUPERVISOR_SIGTERM_CHILD_TEST: &str = "tests::macos_supervisor_sigterm_child";
#[cfg(target_os = "macos")]
const MACOS_SUPERVISOR_SIGTERM_BOUND: Duration = Duration::from_secs(3);

#[cfg(target_os = "macos")]
fn macos_supervisor_sigterm_receipt(
    phase: &str,
    run: u8,
    bind: SocketAddr,
    fixture_heads_sha256: &str,
    cursor_proof: &str,
    collector_stopped: usize,
    listener_stopped: usize,
) -> String {
    format!(
        "phase={phase}\nrun={run}\nbind={bind}\nfixture_heads_sha256={fixture_heads_sha256}\ncursor_proof={cursor_proof}\nfake_collector_outbound=0\ncollector_stopped={collector_stopped}\nlistener_stopped={listener_stopped}\n"
    )
}

#[cfg(target_os = "macos")]
async fn wait_for_macos_supervisor_sigterm_receipt(
    receipt_path: &Path,
    expected: &str,
) -> Result<(), String> {
    tokio::time::timeout(MACOS_SUPERVISOR_SIGTERM_BOUND, async {
        loop {
            match fs::read(receipt_path) {
                Ok(receipt) if receipt == expected.as_bytes() => return Ok(()),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "read SIGTERM child receipt {}: {error}",
                        receipt_path.display()
                    ));
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .map_err(|_| {
        format!(
            "timed out waiting for SIGTERM child receipt {}",
            receipt_path.display()
        )
    })?
}

#[cfg(target_os = "macos")]
async fn wait_for_macos_supervisor_sigterm_child(
    child: &mut Child,
    phase: &str,
) -> Result<std::process::ExitStatus, String> {
    tokio::time::timeout(MACOS_SUPERVISOR_SIGTERM_BOUND, async {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => tokio::time::sleep(Duration::from_millis(5)).await,
                Err(error) => {
                    return Err(format!("inspect SIGTERM child during {phase}: {error}"));
                }
            }
        }
    })
    .await
    .map_err(|_| format!("SIGTERM child did not exit during {phase}"))?
}

#[cfg(target_os = "macos")]
async fn reap_macos_supervisor_sigterm_child(child: &mut Child) -> Result<(), String> {
    if child
        .try_wait()
        .map_err(|error| format!("inspect SIGTERM child cleanup: {error}"))?
        .is_some()
    {
        return Ok(());
    }
    let _ = child.kill();
    wait_for_macos_supervisor_sigterm_child(child, "forced cleanup")
        .await
        .map(|_| ())
}

#[cfg(target_os = "macos")]
fn send_sigterm_to_macos_supervisor_child(child: &Child) -> Result<(), String> {
    let raw_pid =
        i32::try_from(child.id()).map_err(|_| "SIGTERM child PID does not fit i32".to_owned())?;
    let pid = rustix::process::Pid::from_raw(raw_pid)
        .ok_or_else(|| "SIGTERM child has an invalid PID".to_owned())?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)
        .map_err(|error| format!("send SIGTERM to child: {error}"))
}

#[cfg(target_os = "macos")]
async fn run_macos_supervisor_sigterm_child_cycle(
    test_binary: &Path,
    fixture_path: &Path,
    bind: SocketAddr,
    run: u8,
) -> Result<(), String> {
    let fixture = fs::read(fixture_path)
        .map_err(|error| format!("read stable fixture {}: {error}", fixture_path.display()))?;
    let fixture_heads_sha256 = hex::encode(Sha256::digest(&fixture));
    let expected_cursor_proof = supervisor_cursor_proof(&CursorKey::from_bytes([0xB7; 32]));
    let receipt_path = fixture_path.with_extension(format!("sigterm-receipt-{run}"));
    let mut child = ProcessCommand::new(test_binary)
        .args([
            "--ignored",
            "--exact",
            MACOS_SUPERVISOR_SIGTERM_CHILD_TEST,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MACOS_SUPERVISOR_SIGTERM_CHILD_ENV, "1")
        .env(MACOS_SUPERVISOR_SIGTERM_CHILD_BIND_ENV, bind.to_string())
        .env(MACOS_SUPERVISOR_SIGTERM_CHILD_RECEIPT_ENV, &receipt_path)
        .env(MACOS_SUPERVISOR_SIGTERM_CHILD_FIXTURE_ENV, fixture_path)
        .env(MACOS_SUPERVISOR_SIGTERM_CHILD_RUN_ENV, run.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("spawn SIGTERM child: {error}"))?;

    let ready = macos_supervisor_sigterm_receipt(
        "ready",
        run,
        bind,
        &fixture_heads_sha256,
        &expected_cursor_proof,
        0,
        0,
    );
    let completed = macos_supervisor_sigterm_receipt(
        "stopped",
        run,
        bind,
        &fixture_heads_sha256,
        &expected_cursor_proof,
        1,
        1,
    );
    let result = async {
        wait_for_macos_supervisor_sigterm_receipt(&receipt_path, &ready).await?;
        TcpStream::connect_timeout(&bind, Duration::from_millis(250)).map_err(|error| {
            format!("SIGTERM child did not expose its loopback listener: {error}")
        })?;
        match TcpListener::bind(bind) {
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(error) => {
                return Err(format!(
                    "unexpected loopback bind error while SIGTERM child is live: {error}"
                ));
            }
            Ok(listener) => {
                drop(listener);
                return Err("SIGTERM child wrote ready before binding its listener".to_owned());
            }
        }
        send_sigterm_to_macos_supervisor_child(&child)?;
        let status = wait_for_macos_supervisor_sigterm_child(&mut child, "SIGTERM").await?;
        if !status.success() {
            return Err(format!("SIGTERM child exited unsuccessfully: {status}"));
        }
        wait_for_macos_supervisor_sigterm_receipt(&receipt_path, &completed).await?;
        let after = fs::read(fixture_path).map_err(|error| {
            format!(
                "read fixture after SIGTERM child {}: {error}",
                fixture_path.display()
            )
        })?;
        if after != fixture {
            return Err("SIGTERM child changed stable fixture heads".to_owned());
        }
        Ok(())
    }
    .await;

    if result.is_err() {
        reap_macos_supervisor_sigterm_child(&mut child).await?;
    }
    result
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "re-exec child of macos_supervisor_sigterm_releases_listener_and_reruns_same_fixture; not a standalone test"]
async fn macos_supervisor_sigterm_child() {
    if std::env::var_os(MACOS_SUPERVISOR_SIGTERM_CHILD_ENV).is_none() {
        return;
    }

    let bind = std::env::var(MACOS_SUPERVISOR_SIGTERM_CHILD_BIND_ENV)
        .expect("SIGTERM child bind")
        .parse::<SocketAddr>()
        .expect("SIGTERM child loopback bind");
    assert!(bind.ip().is_loopback(), "SIGTERM child must bind loopback");
    let receipt_path = PathBuf::from(
        std::env::var_os(MACOS_SUPERVISOR_SIGTERM_CHILD_RECEIPT_ENV)
            .expect("SIGTERM child receipt path"),
    );
    let fixture_path = PathBuf::from(
        std::env::var_os(MACOS_SUPERVISOR_SIGTERM_CHILD_FIXTURE_ENV)
            .expect("SIGTERM child fixture path"),
    );
    let run = std::env::var(MACOS_SUPERVISOR_SIGTERM_CHILD_RUN_ENV)
        .expect("SIGTERM child run")
        .parse::<u8>()
        .expect("SIGTERM child run number");
    let fixture_heads_sha256 = hex::encode(Sha256::digest(
        fs::read(&fixture_path).expect("read SIGTERM child stable fixture"),
    ));
    let cursor_key = CursorKey::from_bytes([0xB7; 32]);
    let cursor_proof = supervisor_cursor_proof(&cursor_key);
    let ready = macos_supervisor_sigterm_receipt(
        "ready",
        run,
        bind,
        &fixture_heads_sha256,
        &cursor_proof,
        0,
        0,
    );
    let stopped = macos_supervisor_sigterm_receipt(
        "stopped",
        run,
        bind,
        &fixture_heads_sha256,
        &cursor_proof,
        1,
        1,
    );
    let collector_stopped = Arc::new(AtomicUsize::new(0));
    let server_stopped = Arc::new(AtomicUsize::new(0));
    let collector_stopped_for_task = Arc::clone(&collector_stopped);
    let server_stopped_for_task = Arc::clone(&server_stopped);
    let ready_receipt_path = receipt_path.clone();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM listener in child");

    run_macos_serve_supervisor(
        true,
        move |ready_tx, shutdown| async move {
            ready_tx
                .send(cursor_key)
                .map_err(|_| std::io::Error::other("SIGTERM child lost collector readiness"))?;
            let _ = shutdown.await;
            collector_stopped_for_task.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
        move |received_cursor_key, shutdown| async move {
            let received_cursor_key = received_cursor_key.ok_or_else(|| {
                std::io::Error::other("SIGTERM child server started without collector cursor")
            })?;
            if supervisor_cursor_proof(&received_cursor_key) != cursor_proof {
                return Err(std::io::Error::other(
                    "SIGTERM child server did not receive collector cursor",
                ));
            }
            let listener = tokio::net::TcpListener::bind(bind).await?;
            fs::write(&ready_receipt_path, ready.as_bytes())?;
            let mut shutdown = shutdown;
            loop {
                tokio::select! {
                    _ = &mut shutdown => break,
                    accepted = listener.accept() => {
                        let _ = accepted?;
                    }
                }
            }
            drop(listener);
            server_stopped_for_task.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
        async move {
            let _ = sigterm.recv().await;
            MacServeControl::Shutdown
        },
    )
    .await
    .expect("SIGTERM child supervisor shutdown");

    assert_eq!(collector_stopped.load(Ordering::SeqCst), 1);
    assert_eq!(server_stopped.load(Ordering::SeqCst), 1);
    fs::write(receipt_path, stopped.as_bytes()).expect("write SIGTERM child stopped receipt");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_supervisor_sigterm_releases_listener_and_reruns_same_fixture() {
    let temporary = tempfile::tempdir().expect("SIGTERM lifecycle temporary root");
    let fixture_path = temporary.path().join("stable-installation-heads");
    let fixture = format!(
        "installation_id={}\nhead_id={}\n",
        Uuid::new_v4(),
        Uuid::new_v4()
    );
    fs::write(&fixture_path, fixture.as_bytes()).expect("write stable fixture heads");
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve loopback");
    let bind = reservation.local_addr().expect("reserved loopback address");
    drop(reservation);
    let test_binary = std::env::current_exe().expect("current test executable");

    run_macos_supervisor_sigterm_child_cycle(&test_binary, &fixture_path, bind, 1)
        .await
        .expect("first SIGTERM lifecycle child");
    let rebound = TcpListener::bind(bind).expect("listener rebinds after first SIGTERM");
    drop(rebound);

    run_macos_supervisor_sigterm_child_cycle(&test_binary, &fixture_path, bind, 2)
        .await
        .expect("second SIGTERM lifecycle child");
    let rebound = TcpListener::bind(bind).expect("listener rebinds after second SIGTERM");
    drop(rebound);
    assert_eq!(
        fs::read(&fixture_path).expect("read stable fixture after rerun"),
        fixture.as_bytes(),
        "SIGTERM lifecycle changed stable installation/head fixture"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_supervisor_zero_cadence_never_constructs_collector() {
    let collector_calls = Arc::new(AtomicUsize::new(0));
    let collector_calls_for_factory = Arc::clone(&collector_calls);
    let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
    let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
    let (control_tx, control_rx) = tokio::sync::oneshot::channel();

    let supervisor = tokio::spawn(run_macos_serve_supervisor(
        false,
        move |_ready, _shutdown| {
            collector_calls_for_factory.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        },
        move |cursor_key, shutdown| async move {
            assert!(
                cursor_key.is_none(),
                "collector-disabled Serve received a cursor key"
            );
            let _ = server_started_tx.send(());
            let _ = shutdown.await;
            let _ = server_stopped_tx.send(());
            Ok(())
        },
        async move { control_rx.await.expect("test control") },
    ));

    server_started_rx.await.expect("server started");
    assert_eq!(collector_calls.load(Ordering::SeqCst), 0);
    assert!(control_tx.send(MacServeControl::Shutdown).is_ok());
    supervisor
        .await
        .expect("supervisor task")
        .expect("clean API-only shutdown");
    server_stopped_rx
        .await
        .expect("server stopped before return");
    assert_eq!(collector_calls.load(Ordering::SeqCst), 0);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_supervisor_waits_for_ready_cursor_before_constructing_server() {
    let expected_key = CursorKey::from_bytes([61; 32]);
    let expected_proof = supervisor_cursor_proof(&expected_key);
    let (collector_started_tx, collector_started_rx) = tokio::sync::oneshot::channel();
    let (allow_ready_tx, allow_ready_rx) = tokio::sync::oneshot::channel();
    let (collector_stopped_tx, collector_stopped_rx) = tokio::sync::oneshot::channel();
    let (server_started_tx, mut server_started_rx) = tokio::sync::oneshot::channel();
    let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
    let (control_tx, control_rx) = tokio::sync::oneshot::channel();

    let supervisor = tokio::spawn(run_macos_serve_supervisor(
        true,
        move |ready, shutdown| async move {
            let _ = collector_started_tx.send(());
            let _ = allow_ready_rx.await;
            ready
                .send(expected_key)
                .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
            let _ = shutdown.await;
            let _ = collector_stopped_tx.send(());
            Ok(())
        },
        move |cursor_key, shutdown| async move {
            let cursor_key = cursor_key.expect("collector cursor key");
            let _ = server_started_tx.send(supervisor_cursor_proof(&cursor_key));
            let _ = shutdown.await;
            let _ = server_stopped_tx.send(());
            Ok(())
        },
        async move { control_rx.await.expect("test control") },
    ));

    collector_started_rx.await.expect("collector started");
    assert!(
        server_started_rx.try_recv().is_err(),
        "server started before collector readiness"
    );
    assert!(allow_ready_tx.send(()).is_ok());
    assert_eq!(
        server_started_rx
            .await
            .expect("server started after readiness"),
        expected_proof,
        "server did not receive the collector's exact cursor key"
    );

    assert!(control_tx.send(MacServeControl::Shutdown).is_ok());
    supervisor
        .await
        .expect("supervisor task")
        .expect("clean shutdown");
    collector_stopped_rx
        .await
        .expect("collector stopped before return");
    server_stopped_rx
        .await
        .expect("server stopped before return");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_supervisor_server_error_stops_and_awaits_collector() {
    let (collector_stopped_tx, collector_stopped_rx) = tokio::sync::oneshot::channel();
    let (server_finished_tx, server_finished_rx) = tokio::sync::oneshot::channel();
    let result = run_macos_serve_supervisor(
        true,
        move |ready, shutdown| async move {
            ready
                .send(CursorKey::from_bytes([62; 32]))
                .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
            let _ = shutdown.await;
            let _ = collector_stopped_tx.send(());
            Ok(())
        },
        move |_cursor_key, _shutdown| async move {
            let _ = server_finished_tx.send(());
            Err(std::io::Error::other("test server failure"))
        },
        pending(),
    )
    .await
    .expect_err("server failure returns");

    assert!(result.to_string().contains("test server failure"));
    server_finished_rx.await.expect("server completed");
    collector_stopped_rx
        .await
        .expect("collector stopped before server error returned");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_supervisor_collector_error_stops_and_awaits_server() {
    let (release_collector_tx, release_collector_rx) = tokio::sync::oneshot::channel();
    let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
    let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
    let supervisor = tokio::spawn(run_macos_serve_supervisor(
        true,
        move |ready, _shutdown| async move {
            ready
                .send(CursorKey::from_bytes([63; 32]))
                .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
            let _ = release_collector_rx.await;
            Err(std::io::Error::other("test collector failure"))
        },
        move |_cursor_key, shutdown| async move {
            let _ = server_started_tx.send(());
            let _ = shutdown.await;
            let _ = server_stopped_tx.send(());
            Ok(())
        },
        pending(),
    ));

    server_started_rx
        .await
        .expect("server started after readiness");
    assert!(release_collector_tx.send(()).is_ok());
    let result = supervisor
        .await
        .expect("supervisor task")
        .expect_err("collector failure returns");
    assert!(result.to_string().contains("test collector failure"));
    server_stopped_rx
        .await
        .expect("server stopped before collector error returned");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_supervisor_admission_invalidation_stops_and_awaits_workers() {
    let (collector_stopped_tx, collector_stopped_rx) = tokio::sync::oneshot::channel();
    let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
    let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
    let (control_tx, control_rx) = tokio::sync::oneshot::channel();
    let supervisor = tokio::spawn(run_macos_serve_supervisor(
        true,
        move |ready, shutdown| async move {
            ready
                .send(CursorKey::from_bytes([64; 32]))
                .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
            let _ = shutdown.await;
            let _ = collector_stopped_tx.send(());
            Ok(())
        },
        move |_cursor_key, shutdown| async move {
            let _ = server_started_tx.send(());
            let _ = shutdown.await;
            let _ = server_stopped_tx.send(());
            Ok(())
        },
        async move { control_rx.await.expect("test control") },
    ));

    server_started_rx.await.expect("server started");
    assert!(
        control_tx
            .send(MacServeControl::AdmissionInvalidated(
                std::io::Error::other("test admission invalidated",)
            ))
            .is_ok()
    );
    let result = supervisor
        .await
        .expect("supervisor task")
        .expect_err("admission invalidation returns");
    assert!(result.to_string().contains("test admission invalidated"));
    collector_stopped_rx
        .await
        .expect("collector stopped before invalidation returned");
    server_stopped_rx
        .await
        .expect("server stopped before invalidation returned");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_supervisor_shutdown_stops_and_awaits_workers() {
    let (collector_stopped_tx, collector_stopped_rx) = tokio::sync::oneshot::channel();
    let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
    let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
    let (control_tx, control_rx) = tokio::sync::oneshot::channel();
    let supervisor = tokio::spawn(run_macos_serve_supervisor(
        true,
        move |ready, shutdown| async move {
            ready
                .send(CursorKey::from_bytes([65; 32]))
                .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
            let _ = shutdown.await;
            let _ = collector_stopped_tx.send(());
            Ok(())
        },
        move |_cursor_key, shutdown| async move {
            let _ = server_started_tx.send(());
            let _ = shutdown.await;
            let _ = server_stopped_tx.send(());
            Ok(())
        },
        async move { control_rx.await.expect("test control") },
    ));

    server_started_rx.await.expect("server started");
    assert!(control_tx.send(MacServeControl::Shutdown).is_ok());
    supervisor
        .await
        .expect("supervisor task")
        .expect("shutdown returns after both workers");
    collector_stopped_rx
        .await
        .expect("collector stopped before shutdown returned");
    server_stopped_rx
        .await
        .expect("server stopped before shutdown returned");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_supervisor_shutdown_aborts_uncooperative_worker_after_stop_bound() {
    let (drops_tx, mut drops_rx) = tokio::sync::mpsc::unbounded_channel();
    let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
    let (server_stopped_tx, server_stopped_rx) = tokio::sync::oneshot::channel();
    let (control_tx, control_rx) = tokio::sync::oneshot::channel();
    let supervisor = tokio::spawn(run_macos_serve_supervisor(
        true,
        move |ready, _shutdown| async move {
            let _drop_witness = MacServeDropWitness {
                label: "collector",
                drops: drops_tx,
            };
            ready
                .send(CursorKey::from_bytes([67; 32]))
                .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
            pending::<()>().await;
            Ok(())
        },
        move |_cursor_key, shutdown| async move {
            let _ = server_started_tx.send(());
            let _ = shutdown.await;
            let _ = server_stopped_tx.send(());
            Ok(())
        },
        async move { control_rx.await.expect("test control") },
    ));

    server_started_rx.await.expect("server started");
    assert!(control_tx.send(MacServeControl::Shutdown).is_ok());
    let error = supervisor
        .await
        .expect("supervisor task")
        .expect_err("uncooperative collector times out");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        error
            .get_ref()
            .is_some_and(|source| source.is::<MacServeWorkerStopTimeout>()),
        "stop timeout lost its typed source"
    );
    assert!(error.to_string().contains("collector worker did not stop"));
    server_stopped_rx
        .await
        .expect("cooperative server stopped before timeout returned");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), drops_rx.recv())
            .await
            .expect("collector abort timeout"),
        Some("collector"),
        "uncooperative collector was not aborted and dropped"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_supervisor_cancellation_aborts_owned_workers_without_detaching() {
    let (drops_tx, mut drops_rx) = tokio::sync::mpsc::unbounded_channel();
    let collector_drops = drops_tx.clone();
    let server_drops = drops_tx.clone();
    let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
    let supervisor = tokio::spawn(run_macos_serve_supervisor(
        true,
        move |ready, _shutdown| async move {
            let _drop_witness = MacServeDropWitness {
                label: "collector",
                drops: collector_drops,
            };
            ready
                .send(CursorKey::from_bytes([66; 32]))
                .map_err(|_| std::io::Error::other("test readiness receiver disappeared"))?;
            pending::<()>().await;
            Ok(())
        },
        move |_cursor_key, _shutdown| async move {
            let _drop_witness = MacServeDropWitness {
                label: "server",
                drops: server_drops,
            };
            let _ = server_started_tx.send(());
            pending::<()>().await;
            Ok(())
        },
        pending(),
    ));

    server_started_rx.await.expect("server started");
    supervisor.abort();
    assert!(
        supervisor
            .await
            .expect_err("supervisor cancelled")
            .is_cancelled(),
        "cancellation did not terminate the supervisor"
    );

    let mut dropped = vec![
        tokio::time::timeout(Duration::from_secs(1), drops_rx.recv())
            .await
            .expect("collector/server drop timeout")
            .expect("drop witness"),
        tokio::time::timeout(Duration::from_secs(1), drops_rx.recv())
            .await
            .expect("collector/server drop timeout")
            .expect("drop witness"),
    ];
    dropped.sort_unstable();
    assert_eq!(dropped, ["collector", "server"]);
}

#[tokio::test]
async fn doctor_does_not_create_or_initialize_a_missing_data_directory() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data_dir = temporary.path().join("missing-hub-state");
    let config_path = temporary.path().join("config.toml");
    fs::write(
        &config_path,
        format!("data_dir = {:?}\nbind = '127.0.0.1:18443'\n", data_dir),
    )
    .expect("write config");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
        .expect("make test config private");

    let error = run(Cli {
        config: Some(config_path),
        command: Command::Doctor,
    })
    .await
    .expect_err("doctor must fail on missing state");

    assert!(error.to_string().contains("cannot inspect hub catalogue"));
    assert!(!data_dir.exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn doctor_reports_inventory_and_does_not_delete_tokens() {
    use tesatlas_hub::auth::credential_sign_in as credentials;
    let (temporary, store) = v2_credential_pair_fixture(true);
    assert!(credentials::record_provider_admission_for_test(temporary.path()).expect("C5 integrity record"));
    let config = temporary.path().join("config.toml");
    fs::write(&config, format!("data_dir = {:?}\nbind = '127.0.0.1:18443'\n[collector]\ninterval_seconds = 0\n", temporary.path())).expect("config");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).expect("private config");
    store.checkpoint_catalogue_for_immutable_read().expect("checkpoint");
    run(Cli { config: Some(config), command: Command::Doctor }).await.expect("doctor");
    assert!(credentials::pair_matches_for_test(temporary.path(), false, "access-0001", "refresh-0001").expect("Owner retained"));
    // The stored Fleet row is kept (counted below) and still loads: it is the
    // Hub's own grant (M09-T11). A supplied Fleet row can no longer be stored
    // (the un-park barrier); its refusal at load is the library's
    // `fleet_preflight_rejects_a_pair_without_the_recorded_scope_witness`.
    assert!(credentials::pair_matches_for_test(temporary.path(), true, "access-0002", "refresh-0002").expect("Fleet retained"), "Fleet row retained");
    let inventory = store.catalogue_inventory().expect("inventory");
    assert_eq!(inventory.journal_mode, "wal");
    assert_eq!((inventory.owner_api_credential_rows, inventory.fleet_api_credential_rows), (1, 1));
}

/// A store with a pending WAL witness; `None` gives it the catalogue's mode, as SQLite does.
fn store_with_pending_wal(mode: Option<u32>) -> (tempfile::TempDir, PathBuf) {
    let temporary = tempfile::tempdir().expect("temporary directory");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).expect("protect temporary directory");
    HubStore::initialize(temporary.path()).expect("store initializes");
    let wal = temporary.path().join("hub.sqlite-wal");
    fs::write(&wal, b"pending").expect("create pending WAL witness");
    let catalogue = || fs::metadata(temporary.path().join("hub.sqlite")).expect("catalogue").permissions();
    fs::set_permissions(&wal, mode.map_or_else(catalogue, fs::Permissions::from_mode)).expect("WAL mode");
    (temporary, wal)
}

#[test]
fn immutable_diagnostic_waits_for_a_transient_wal() {
    let (temporary, wal) = store_with_pending_wal(None);
    let mut waits = 0;
    run_immutable_diagnostic_with(temporary.path(), |store| Ok(store.catalogue_check()?), || {
        waits += 1;
        if wal.exists() {
            fs::remove_file(&wal).expect("settle WAL");
        }
    })
    .expect("diagnostic opens after WAL settles");
    assert_eq!(waits, 1);
}

#[test]
fn immutable_diagnostic_refuses_a_wal_with_an_unsafe_mode() {
    let (temporary, wal) = store_with_pending_wal(Some(0o644));
    let mut waits = 0;
    let error = run_immutable_diagnostic_with(temporary.path(), |store| Ok(store.catalogue_check()?), || waits += 1)
        .expect_err("an unsafe WAL is refused, not waited for");
    assert!(matches!(error.downcast_ref::<tesatlas_hub::db::StoreError>(), Some(tesatlas_hub::db::StoreError::UnsafeSharedSqlite(path)) if path == &wal), "unexpected refusal: {error}");
    assert_eq!((waits, fs::read(&wal).expect("WAL kept")), (0, b"pending".to_vec()));
}

#[test]
fn immutable_diagnostic_retries_a_snapshot_changed_during_checks() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
        .expect("protect temporary directory");
    HubStore::initialize(temporary.path()).expect("store initializes");
    let database = temporary.path().join("hub.sqlite");
    let original_bytes = fs::metadata(&database).expect("catalogue metadata").len();
    let mut runs = 0;
    let mut waits = 0;

    run_immutable_diagnostic_with(
        temporary.path(),
        |store| {
            runs += 1;
            store.catalogue_check()?;
            if runs == 1 {
                let mut catalogue = fs::OpenOptions::new().append(true).open(&database)?;
                catalogue.write_all(b"changed")?;
                catalogue.sync_all()?;
            }
            Ok(())
        },
        || {
            waits += 1;
            fs::OpenOptions::new()
                .write(true)
                .open(&database)
                .expect("open changed catalogue")
                .set_len(original_bytes)
                .expect("restore catalogue size");
        },
    )
    .expect("diagnostic retries a changed snapshot");

    assert_eq!(runs, 2);
    assert_eq!(waits, 1);
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[tokio::test]
async fn serve_fails_before_initialising_state_on_an_unsupported_platform() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data_dir = temporary.path().join("missing-hub-state");
    let config_path = temporary.path().join("config.toml");
    fs::write(
        &config_path,
        format!("data_dir = {:?}\nbind = '127.0.0.1:18443'\n", data_dir),
    )
    .expect("write config");

    let error = run(Cli {
        config: Some(config_path),
        command: Command::Serve,
    })
    .await
    .expect_err("unsupported Serve must fail explicitly");

    assert!(error.to_string().contains("not yet supported"));
    assert!(!data_dir.exists());
}

#[test]
fn observation_commands_parse_their_machine_readable_inputs() {
    let watermark =
        Cli::try_parse_from(["tesatlas-hub", "observation-watermark", "--car-id", "17"])
            .expect("watermark CLI");
    assert!(matches!(
        watermark.command,
        Command::ObservationWatermark { car_id: 17 }
    ));

    let verify = Cli::try_parse_from([
        "tesatlas-hub",
        "verify-observation",
        "--car-id",
        "17",
        "--watermark",
        "42",
    ])
    .expect("verification CLI");
    assert!(matches!(
        verify.command,
        Command::VerifyObservation {
            car_id: 17,
            watermark: 42
        }
    ));
}

#[test]
fn native_control_commands_parse_bounded_values() {
    let settings = Cli::try_parse_from([
        "tesatlas-hub",
        "control",
        "settings",
        "--enabled",
        "false",
        "--suspend-min",
        "12",
    ])
    .expect("settings CLI");
    assert!(matches!(
        settings.command,
        Command::Control {
            command: ControlCommand::Settings {
                enabled: Some(false),
                suspend_min: Some(12),
                ..
            },
            ..
        }
    ));

    assert!(
        Cli::try_parse_from(["tesatlas-hub", "control", "export-gpx", "--drive-id", "0",])
            .is_err()
    );

    let sign_out =
        Cli::try_parse_from(["tesatlas-hub", "control", "sign-out"]).expect("sign-out CLI");
    assert!(matches!(
        sign_out.command,
        Command::Control {
            command: ControlCommand::SignOut,
            ..
        }
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn observe_command_requires_positive_duration() {
    let cli = Cli::try_parse_from(["tesatlas-hub", "observe", "--duration-seconds", "3600"])
        .expect("observe CLI");
    assert!(matches!(
        cli.command,
        Command::Observe {
            duration_seconds: 3600
        }
    ));
    assert!(Cli::try_parse_from(["tesatlas-hub", "observe", "--duration-seconds", "0",]).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn setup_command_takes_only_a_positive_optional_vehicle() {
    let cli = Cli::try_parse_from(["tesatlas-hub", "setup", "--vehicle-id", "70"]).expect("setup CLI");
    assert!(matches!(cli.command, Command::Setup { vehicle_id: Some(70), all_vehicles: false }));
    assert!(matches!(
        Cli::try_parse_from(["tesatlas-hub", "setup"]).expect("setup CLI").command,
        Command::Setup { vehicle_id: None, all_vehicles: false }
    ));
    assert!(Cli::try_parse_from(["tesatlas-hub", "setup", "--vehicle-id", "0"]).is_err());
    assert!(Cli::try_parse_from(["tesatlas-hub", "setup", "--all-vehicles", "--vehicle-id", "70"]).is_err());
    for removed in [["--access-token-file", "access"], ["--refresh-token-file", "refresh"], ["--tokens-stdin", "x"], ["--sign-in", "x"]] {
        let mut args = vec!["tesatlas-hub", "setup"];
        args.extend(removed);
        assert!(Cli::try_parse_from(args).is_err(), "{removed:?}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn sign_out_clears_both_providers_but_preserves_cursor_key() {
    use tesatlas_hub::auth::credential_sign_in as credentials;
    let (temporary, store) = v2_credential_pair_fixture(true);
    let cursor = super::load_or_create_cursor_key(temporary.path()).expect("cursor");
    let cursor_proof = supervisor_cursor_proof(&cursor);
    let key_path = temporary.path().join("secrets").join("credential-keys");
    let keys = zeroize::Zeroizing::new(fs::read(&key_path).expect("keyring"));
    let scope = credentials::SignInScope::open_for_test(temporary.path(), true).expect("sign-out process");
    clear_provider_credentials(scope.entry()).expect("sign out");
    scope.release().expect("release");
    let status = credentials::credential_status(&store, temporary.path(), 1_800_000_000_000).expect("status");
    assert!(!status.legacy_credentials.present && !status.fleet_credentials.present);
    assert!(keys.as_slice() == fs::read(&key_path).expect("retained keyring").as_slice());
    assert!(supervisor_cursor_proof(&super::load_or_create_cursor_key(temporary.path()).expect("cursor retained")) == cursor_proof);
}

#[cfg(target_os = "macos")]
#[test]
fn sign_out_attempts_legacy_removal_after_fleet_key_failure() {
    use tesatlas_hub::auth::credential_sign_in as credentials;
    let (temporary, store) = v2_credential_pair_fixture(true);
    // Per-slot key files are gone. A Fleet-only clear failure must leave the
    // other clear committed, in the required Owner-then-Fleet order.
    let connection = rusqlite::Connection::open(store.database_path()).expect("fixture observer");
    connection.execute_batch("CREATE TRIGGER fixture_fleet_clear_refusal BEFORE DELETE ON credential_pairs WHEN (SELECT count(*) FROM credential_pairs WHERE slot='owner_api')=0 BEGIN SELECT RAISE(ABORT,'fleet_clear_refused'); END;").expect("Fleet fault");
    let scope = credentials::SignInScope::open_for_test(temporary.path(), true).expect("sign-out process");
    let error = clear_provider_credentials(scope.entry()).expect_err("Fleet failure reported");
    assert!(error.to_string().contains("fleet_api:"));
    scope.release().expect("release");
    let status = credentials::credential_status(&store, temporary.path(), 1_800_000_000_000).expect("status");
    assert!(!status.legacy_credentials.present, "Owner clear still commits");
    assert!(status.fleet_credentials.present, "failed Fleet clear retains its row");
}

/// Setup and migration token inputs are refused and change no stored pair.
#[cfg(target_os = "macos")]
#[test]
fn source_credential_disabled_setup_and_migrate_token_inputs_change_nothing() {
    use tesatlas_hub::auth::credential_sign_in as credentials;
    let (temporary, store) = v2_credential_pair_fixture(false);
    let before = credentials::credential_status(&store, temporary.path(), 1_800_000_000_000).expect("before");
    for kind in [credentials::OwnerEntryKind::Direct, credentials::OwnerEntryKind::Provider, credentials::OwnerEntryKind::SourceToken] {
        let access = zeroize::Zeroizing::new("access-0003".to_owned());
        let refused = credentials::prepare_owner_entry(kind, access, Some(zeroize::Zeroizing::new("refresh-0003".to_owned())), false).unwrap_err();
        assert_eq!(refused.code(), "source_credential_disabled");
    }
    // The command tree has no token input at all.
    for args in [vec!["setup", "--tokens-stdin"], vec!["setup-fleet", "--sign-in"], vec!["setup", "--access-token-file", "a", "--refresh-token-file", "r"]] {
        let mut all = vec!["tesatlas-hub"];
        all.extend(args);
        assert!(Cli::try_parse_from(all).is_err());
    }
    let after = credentials::credential_status(&store, temporary.path(), 1_800_000_000_000).expect("after");
    assert_eq!(after, before, "no stored pair changed");
    // The retained Fleet row is the Hub's own grant (M09-T11): it is kept and
    // unchanged. A supplied row can no longer be stored to be refused at load.
    assert!(credentials::pair_matches_for_test(temporary.path(), true, "access-0002", "refresh-0002").expect("Fleet pair retained"), "Fleet pair unchanged");
}

#[cfg(target_os = "linux")]
#[test]
fn service_commands_parse_without_live_store_admission() {
    use super::ServiceCommand;
    for (name, expected) in [
        ("status", "status"),
        ("start", "start"),
        ("stop", "stop"),
        ("restart", "restart"),
    ] {
        let cli = Cli::try_parse_from(["tesatlas-hub", "service", name]).expect("service CLI");
        let Command::Service { command } = cli.command else {
            panic!("service command")
        };
        let actual = match command {
            ServiceCommand::Status => "status",
            ServiceCommand::Start => "start",
            ServiceCommand::Stop => "stop",
            ServiceCommand::Restart => "restart",
        };
        assert_eq!(actual, expected);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn long_lived_and_sensitive_commands_require_the_instance_lock() {
    use tesatlas_hub::runtime::operator::admission::requires_user_hub_admission as command_requires_user_hub_admission;
    assert!(command_requires_user_hub_admission(&Command::Init));
    assert!(command_requires_user_hub_admission(&Command::Serve));
    assert!(command_requires_user_hub_admission(&Command::Observe {
        duration_seconds: 1,
    }));
    assert!(command_requires_user_hub_admission(&Command::Migrate {
        source_key: None,
        new_import_source: false,
        source: "postgresql://localhost/sourcedb".to_owned(),
        car_id: 1,
        postgres_password_file: PathBuf::from("password"),
        history_only: false,
        online_snapshot: false,
        accept_schema_evidence: true,
    }));
    assert!(command_requires_user_hub_admission(&Command::Pair {
        label: "test phone".to_owned(),
        expires_in_seconds: 900,
        json: false,
        vehicles: Vec::new(),
        all_vehicles: false,
    }));
    assert!(command_requires_user_hub_admission(&Command::Repair));
    assert!(command_requires_user_hub_admission(&Command::Backup {
        destination: PathBuf::from("backup"),
    }));
    // Setup and credential recovery are forwarded to the running service and
    // never take the lifetime lock themselves (ruling R5); Fleet is refused.
    for command in [
        Command::Setup { vehicle_id: None, all_vehicles: false },
        Command::SetupFleet { step: None, vehicle_id: None, all_vehicles: false },
        Command::ExportRecoveryCredentials {
            destination: PathBuf::from("credentials.tthcr"),
            recovery_key_file: PathBuf::from("recovery.key"),
        },
        Command::RestoreRecoveryCredentials {
            source: PathBuf::from("credentials.tthcr"),
            recovery_key_file: PathBuf::from("recovery.key"),
        },
    ] {
        assert!(!command_requires_user_hub_admission(&command), "{command:?}");
    }
    assert!(!command_requires_user_hub_admission(&Command::Doctor));
    assert!(!command_requires_user_hub_admission(&Command::Legal));
    assert!(!command_requires_user_hub_admission(&Command::Source));
    assert!(!command_requires_user_hub_admission(&Command::Status));
    assert!(!command_requires_user_hub_admission(&Command::Preflight {
        history_only: true
    }));
    #[cfg(target_os = "macos")]
    assert!(!command_requires_user_hub_admission(
        &Command::ServePreflight {
            mode: DevelopmentServeModeArgument::Standalone,
            history_only: false,
        }
    ));
    assert!(!command_requires_user_hub_admission(&Command::Control {
        vehicle_id: None,
        command: ControlCommand::Pause,
    }));
}

#[cfg(target_os = "macos")]
#[test]
fn source_run_serve_preflight_cli_requires_an_explicit_known_mode() {
    let cli = Cli::try_parse_from([
        "tesatlas-hub",
        "--config",
        "/private/tmp/source-run.toml",
        "serve-preflight",
        "--mode",
        "standalone",
    ])
    .expect("explicit standalone serve preflight");
    assert!(matches!(
        cli.command,
        Command::ServePreflight {
            mode: DevelopmentServeModeArgument::Standalone,
            history_only: false,
        }
    ));
    assert!(
        Cli::try_parse_from(["tesatlas-hub", "serve-preflight", "--mode", "edge"]).is_err(),
        "the retired Edge mode is refused"
    );
    assert!(
        Cli::try_parse_from(["tesatlas-hub", "serve-preflight"]).is_err(),
        "mode is mandatory"
    );
    assert!(
        Cli::try_parse_from(["tesatlas-hub", "serve-preflight", "--mode", "production",]).is_err(),
        "production is not a source-run mode"
    );
    let selected =
        Cli::try_parse_from(["tesatlas-hub", "serve-preflight", "--mode", "private-lan"])
            .expect("explicit private-LAN preflight mode");
    assert!(matches!(
        selected.command,
        Command::ServePreflight {
            mode: DevelopmentServeModeArgument::PrivateLan,
            history_only: false,
        }
    ));
    let history = Cli::try_parse_from([
        "tesatlas-hub",
        "serve-preflight",
        "--mode",
        "standalone",
        "--history-only",
    ])
    .expect("history-only standalone preflight");
    assert!(matches!(
        history.command,
        Command::ServePreflight {
            mode: DevelopmentServeModeArgument::Standalone,
            history_only: true,
        }
    ));
}

fn test_identity(name: &str) -> (String, zeroize::Zeroizing<String>, Vec<u8>) {
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec![name.to_owned()]).expect("test TLS identity");
    (
        cert.pem(),
        zeroize::Zeroizing::new(signing_key.serialize_pem()),
        cert.der().to_vec(),
    )
}

fn write_private_test_file(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::write(path, bytes).expect("write private test file");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .expect("protect private test file");
}

fn write_test_identity(certificate_path: &Path, private_key_path: &Path) -> Vec<u8> {
    let (certificate_pem, private_key_pem, certificate_der) = test_identity("hub.example");
    write_private_test_file(certificate_path, certificate_pem);
    write_private_test_file(private_key_path, private_key_pem.as_bytes());
    certificate_der
}

fn write_test_certificate(certificate_path: &Path) -> Vec<u8> {
    let (certificate_pem, _private_key_pem, certificate_der) = test_identity("hub.example");
    write_private_test_file(certificate_path, certificate_pem);
    certificate_der
}

fn pairing_challenge_count(store: &HubStore) -> i64 {
    store
        .open()
        .expect("open Hub store")
        .query_row("SELECT COUNT(*) FROM pairing_challenges", [], |row| {
            row.get(0)
        })
        .expect("pairing challenge count")
}

struct FlushFailingWriter {
    bytes: zeroize::Zeroizing<Vec<u8>>,
}

impl Default for FlushFailingWriter {
    fn default() -> Self {
        Self {
            bytes: zeroize::Zeroizing::new(Vec::new()),
        }
    }
}

impl Write for FlushFailingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "test presentation sink failed",
        ))
    }
}

struct WriteFailingWriter;

impl Write for WriteFailingWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "test presentation sink failed",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct NeverWriter;

impl Write for NeverWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        panic!("pairing presentation must not be written")
    }

    fn flush(&mut self) -> io::Result<()> {
        panic!("pairing presentation must not be flushed")
    }
}

#[tokio::test]
async fn pairing_certificate_key_and_qr_failures_leave_no_invitation() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("Hub store");
    let certificate = temporary.path().join("leaf.pem");
    let private_key = temporary.path().join("private-key.pem");
    write_test_identity(&certificate, &private_key);
    let missing = temporary.path().join("access-secret-certificate.pem");
    let mut output = zeroize::Zeroizing::new(Vec::new());
    let certificate_error = execute_pairing_at(
        &store,
        PairingCommandInput {
            label: "test phone",
            expires_in_seconds: 900,
            json: false,
            public_url: "https://hub.example/",
            certificate_path: &missing,
            private_key_path: &private_key,
            created_at_ms: 1_000,
            scope: tesatlas_hub::db::PairingScope::AllVehicles,
        },
        &mut *output,
    )
    .await
    .expect_err("missing certificate");
    assert!(matches!(
        &certificate_error,
        PairingCommandError::Certificate(_)
    ));
    assert!(!format!("{certificate_error:?}").contains("access-secret"));
    assert!(!certificate_error.to_string().contains("access-secret"));
    assert_eq!(pairing_challenge_count(&store), 0);

    let mismatched_certificate = temporary.path().join("access-secret-mismatch-cert.pem");
    let mismatched_key = temporary.path().join("access-secret-mismatch-key.pem");
    let (first_certificate_pem, _first_key_pem, _) = test_identity("hub.example");
    let (_second_certificate_pem, second_key_pem, _) = test_identity("hub.example");
    write_private_test_file(&mismatched_certificate, first_certificate_pem);
    write_private_test_file(&mismatched_key, second_key_pem.as_bytes());
    let mismatch_error = execute_pairing_at(
        &store,
        PairingCommandInput {
            label: "test phone",
            expires_in_seconds: 900,
            json: false,
            public_url: "https://hub.example/",
            certificate_path: &mismatched_certificate,
            private_key_path: &mismatched_key,
            created_at_ms: 1_000,
            scope: tesatlas_hub::db::PairingScope::AllVehicles,
        },
        &mut NeverWriter,
    )
    .await
    .expect_err("mismatched certificate and key");
    assert!(matches!(
        mismatch_error,
        PairingCommandError::Certificate(_)
    ));
    assert!(!format!("{mismatch_error:?}").contains("access-secret"));
    assert!(!mismatch_error.to_string().contains("access-secret"));
    assert_eq!(pairing_challenge_count(&store), 0);

    let oversized_endpoint = format!("https://hub.example/{}", "x".repeat(16 * 1024));
    assert!(matches!(
        execute_pairing_at(
            &store,
            PairingCommandInput {
                label: "test phone",
                expires_in_seconds: 900,
                json: false,
                public_url: &oversized_endpoint,
                certificate_path: &certificate,
                private_key_path: &private_key,
                created_at_ms: 1_000,
                scope: tesatlas_hub::db::PairingScope::AllVehicles,
            },
            &mut *output,
        )
        .await,
        Err(PairingCommandError::Presentation)
    ));
    assert_eq!(pairing_challenge_count(&store), 0);
}

#[tokio::test]
async fn pairing_flush_failure_revokes_and_success_persists_once() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("Hub store");
    let certificate = temporary.path().join("leaf.pem");
    let private_key = temporary.path().join("private-key.pem");
    let certificate_der = write_test_identity(&certificate, &private_key);

    let mut write_failure = WriteFailingWriter;
    let error = execute_pairing_at(
        &store,
        PairingCommandInput {
            label: "broken writer",
            expires_in_seconds: 900,
            json: true,
            public_url: "https://hub.example/",
            certificate_path: &certificate,
            private_key_path: &private_key,
            created_at_ms: 500,
            scope: tesatlas_hub::db::PairingScope::AllVehicles,
        },
        &mut write_failure,
    )
    .await
    .expect_err("write failure");
    assert!(matches!(
        error,
        PairingCommandError::Present {
            kind: io::ErrorKind::BrokenPipe
        }
    ));
    assert_eq!(pairing_challenge_count(&store), 0);

    let mut broken = FlushFailingWriter::default();
    let error = execute_pairing_at(
        &store,
        PairingCommandInput {
            label: "broken terminal",
            expires_in_seconds: 900,
            json: true,
            public_url: "https://hub.example/",
            certificate_path: &certificate,
            private_key_path: &private_key,
            created_at_ms: 1_000,
            scope: tesatlas_hub::db::PairingScope::AllVehicles,
        },
        &mut broken,
    )
    .await
    .expect_err("flush failure");
    assert!(matches!(
        error,
        PairingCommandError::Present {
            kind: io::ErrorKind::BrokenPipe
        }
    ));
    assert_eq!(pairing_challenge_count(&store), 0);

    let mut output = zeroize::Zeroizing::new(Vec::new());
    execute_pairing_at(
        &store,
        PairingCommandInput {
            label: "working terminal",
            expires_in_seconds: 900,
            json: true,
            public_url: "https://hub.example/",
            certificate_path: &certificate,
            private_key_path: &private_key,
            created_at_ms: 2_000,
            scope: tesatlas_hub::db::PairingScope::AllVehicles,
        },
        &mut *output,
    )
    .await
    .expect("pairing succeeds");
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct BorrowedPresentation<'a> {
        #[serde(borrow)]
        secret: &'a str,
        #[serde(borrow)]
        tls_pin: &'a str,
    }
    let document: BorrowedPresentation<'_> = serde_json::from_slice(&output).expect("pairing JSON");
    assert!(!document.secret.is_empty());
    assert_eq!(
        document.tls_pin,
        hex::encode(sha2::Sha256::digest(certificate_der))
    );
    assert_eq!(pairing_challenge_count(&store), 1);
}

#[test]
fn pairing_committed_then_error_is_revoked_before_any_output() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("Hub store");
    let invitation = store
        .prepare_pairing("ambiguous terminal", 1_000, 901_000)
        .expect("pairing prepares");
    let marker = zeroize::Zeroizing::new(b"pairing-secret-marker".to_vec());
    let error = persist_and_present_pairing(
        &mut NeverWriter,
        &marker,
        || {
            store.persist_pairing("ambiguous terminal", &invitation)?;
            Err(tesatlas_hub::db::StoreError::PairingRejected)
        },
        || store.revoke_pairing(invitation.pairing_id),
    )
    .expect_err("committed-then-error persistence is ambiguous");
    assert!(matches!(error, PairingCommandError::Persist(_)));
    assert_eq!(pairing_challenge_count(&store), 0);
    let debug = format!("{error:?}");
    let display = error.to_string();
    assert!(!debug.contains("pairing-secret-marker"));
    assert!(!display.contains("pairing-secret-marker"));
}

#[test]
fn pairing_persist_and_revoke_failure_is_typed_and_redacted() {
    let marker = zeroize::Zeroizing::new(b"pairing-secret-marker".to_vec());
    let error = persist_and_present_pairing(
        &mut NeverWriter,
        &marker,
        || Err(tesatlas_hub::db::StoreError::PairingRejected),
        || Err(tesatlas_hub::db::StoreError::PairingRejected),
    )
    .expect_err("persistence and cleanup both fail");
    assert!(matches!(
        error,
        PairingCommandError::PersistAndRevoke { .. }
    ));
    let debug = format!("{error:?}");
    let display = error.to_string();
    assert!(!debug.contains("pairing-secret-marker"));
    assert!(!display.contains("pairing-secret-marker"));
}

#[test]
fn pairing_presentation_and_revoke_failure_is_typed_and_redacted() {
    let marker = zeroize::Zeroizing::new(b"pairing-secret-marker".to_vec());
    let mut writer = FlushFailingWriter::default();
    let error = persist_and_present_pairing(
        &mut writer,
        &marker,
        || Ok(()),
        || Err(tesatlas_hub::db::StoreError::PairingRejected),
    )
    .expect_err("presentation and cleanup fail");
    assert!(matches!(
        error,
        PairingCommandError::PresentAndRevoke {
            kind: io::ErrorKind::BrokenPipe,
            ..
        }
    ));
    let debug = format!("{error:?}");
    let display = error.to_string();
    assert!(!debug.contains("pairing-secret-marker"));
    assert!(!display.contains("pairing-secret-marker"));
}

#[test]
fn leaf_certificate_reader_rejects_symlink_mode_size_and_identity_races() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let target = temporary.path().join("target.pem");
    write_test_certificate(&target);
    let link = temporary.path().join("link.pem");
    symlink(&target, &link).expect("certificate symlink");
    assert!(leaf_certificate_sha256(&link).is_err());

    let unsafe_mode = temporary.path().join("unsafe.pem");
    write_test_certificate(&unsafe_mode);
    fs::set_permissions(&unsafe_mode, fs::Permissions::from_mode(0o622))
        .expect("unsafe certificate mode");
    assert!(leaf_certificate_sha256(&unsafe_mode).is_err());

    let (base, _private_key_pem, _) = test_identity("bounded.example");
    let bounded = temporary.path().join("bounded.pem");
    let mut exact = base.into_bytes();
    exact.resize(MAX_TLS_CERTIFICATE_CHAIN_BYTES, b'\n');
    write_private_test_file(&bounded, &exact);
    leaf_certificate_sha256(&bounded).expect("exact certificate cap accepted");
    exact.push(b'\n');
    write_private_test_file(&bounded, &exact);
    assert!(leaf_certificate_sha256(&bounded).is_err());

    let replaced = temporary.path().join("replaced.pem");
    write_test_certificate(&replaced);
    let old = temporary.path().join("old.pem");
    let replacement_path = replaced.clone();
    assert!(
        leaf_certificate_sha256_after_open(&replaced, || {
            fs::rename(&replacement_path, &old).expect("move opened certificate");
            write_test_certificate(&replacement_path);
        })
        .is_err()
    );

    let mutated = temporary.path().join("mutated.pem");
    let (original_pem, _private_key_pem, _) = test_identity("mutated.example");
    let mut changed_pem = original_pem.as_bytes().to_vec();
    let changed = changed_pem
        .iter_mut()
        .find(|byte| byte.is_ascii_alphanumeric())
        .expect("certificate has mutable text");
    *changed = if *changed == b'A' { b'B' } else { b'A' };
    write_private_test_file(&mutated, original_pem);
    let mutation_path = mutated.clone();
    assert!(
        leaf_certificate_sha256_after_open(&mutated, || {
            std::thread::sleep(Duration::from_millis(2));
            write_private_test_file(&mutation_path, &changed_pem);
        })
        .is_err()
    );

    let (_certificate_pem, private_key_pem, _) = test_identity("key.example");
    let key_target = temporary.path().join("private-key-target.pem");
    write_private_test_file(&key_target, private_key_pem.as_bytes());
    let key_link = temporary.path().join("private-key-link.pem");
    symlink(&key_target, &key_link).expect("private key symlink");
    assert!(read_tls_identity_file(&key_link, MAX_TLS_PRIVATE_KEY_BYTES, true).is_err());
    fs::set_permissions(&key_target, fs::Permissions::from_mode(0o640))
        .expect("unsafe private key mode");
    assert!(read_tls_identity_file(&key_target, MAX_TLS_PRIVATE_KEY_BYTES, true).is_err());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn observe_supervisor_stops_server_after_control_shutdown() {
    run_macos_serve_supervisor(
        false,
        |_ready, _shutdown| async { Ok(()) },
        |_cursor_key, shutdown| async move {
            let _ = shutdown.await;
            Ok(())
        },
        async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            MacServeControl::Shutdown
        },
    )
    .await
    .expect("observe supervisor shutdown");
}

#[test]
fn pairing_uri_encodes_its_endpoint_as_one_query_value() {
    let pin = "a".repeat(64);
    let uri = pairing_uri(
        "https://hub.example/",
        &pin,
        Uuid::nil(),
        "0123456789abcdef",
    )
    .expect("pairing URI");
    assert!(uri.contains("endpoint=https%3A%2F%2Fhub.example%2F"));
    assert!(uri.contains("pairing_id=00000000-0000-0000-0000-000000000000"));
    assert!(uri.contains(&format!("tls_pin={pin}")));
}

#[test]
fn leaf_certificate_pin_uses_rustls_line_aware_first_certificate() {
    let (first_pem, _first_key_pem, first_der) = test_identity("first.example");
    let (second_pem, _second_key_pem, second_der) = test_identity("second.example");
    let temporary = tempfile::tempdir().expect("temporary PEM root");
    let chain = temporary.path().join("chain.pem");
    write_private_test_file(&chain, format!("{first_pem}\n{second_pem}"));

    assert_eq!(
        leaf_certificate_sha256(&chain).expect("first chain leaf pin"),
        hex::encode(sha2::Sha256::digest(first_der))
    );

    let inline_marker = first_pem.replacen(
        "-----BEGIN CERTIFICATE-----",
        "x-----BEGIN CERTIFICATE-----",
        1,
    );
    write_private_test_file(&chain, format!("{inline_marker}\n{second_pem}"));
    assert_eq!(
        leaf_certificate_sha256(&chain).expect("line-aware leaf pin"),
        hex::encode(sha2::Sha256::digest(second_der))
    );
}

#[test]
fn default_pairing_presentation_includes_a_plain_link() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("Hub store");
    let invitation = store
        .prepare_pairing("headless phone", 1_000, 901_000)
        .expect("pairing prepares");
    let uri = pairing_uri(
        "https://hub.example/",
        &"a".repeat(64),
        invitation.pairing_id,
        invitation.secret(),
    )
    .expect("pairing URI");
    let presentation = build_pairing_presentation(
        false,
        "https://hub.example/",
        &"a".repeat(64),
        &uri,
        &invitation,
        900,
        "Grants: every vehicle that is active when this invitation is claimed.",
    )
    .expect("terminal presentation");
    let text = std::str::from_utf8(&presentation).expect("terminal UTF-8");
    assert!(text.starts_with(&format!("Paste this link in Tesatlas:\n{}\n", uri.as_str())));
    assert!(!text.contains("Scan"));
    assert!(!text.chars().any(|c| matches!(c, '█' | '▀' | '▄')));
    assert!(text.ends_with("Expires in 900 seconds.\n"));
}

#[cfg(unix)]
mod lan_tls_setup {
    use std::{
        fs,
        os::unix::fs::PermissionsExt as _,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::super::{
        HubConfig, LAN_TLS_VALIDITY_DAYS, lan_tls_certificate_params, setup_lan_tls,
        valid_lan_host,
    };

    const HOST: &str = "office-mac.local";

    fn hub_fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let root = tempfile::tempdir().expect("temporary Hub");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).expect("private root");
        let data = root.path().join("data");
        fs::create_dir(&data).expect("data folder");
        fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).expect("private data");
        let config = root.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "data_dir = {:?}\nbind = \"127.0.0.1:8080\"\n\n[collector]\ninterval_seconds = 60\n\n[geocoder]\nenabled = false\n",
                data.to_str().expect("UTF-8 path")
            ),
        )
        .expect("config");
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).expect("private config");
        (root, config, data)
    }

    fn mode(path: &std::path::Path) -> u32 {
        fs::symlink_metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    #[test]
    fn lan_host_is_one_lowercase_bonjour_label() {
        for host in ["office-mac.local", "a.local", "mac2.local"] {
            assert!(valid_lan_host(host), "{host}");
        }
        for host in [
            "", ".local", "Office-Mac.local", "office mac.local", "office.mac.local",
            "-mac.local", "mac-.local", "office-mac", "office-mac.lan", "mac.local.",
            "192.168.1.10", "mac_1.local",
        ] {
            assert!(!valid_lan_host(host), "{host:?}");
        }
        assert!(!valid_lan_host(&format!("{}.local", "a".repeat(64))));
        assert!(valid_lan_host(&format!("{}.local", "a".repeat(63))));
    }

    #[test]
    fn lan_identity_lives_397_days_for_the_named_host() {
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let params = lan_tls_certificate_params(HOST, now).expect("parameters");
        assert_eq!(LAN_TLS_VALIDITY_DAYS, 397);
        assert_eq!(params.not_before.unix_timestamp(), 1_800_000_000);
        assert_eq!(
            params.not_after - params.not_before,
            time::Duration::days(397)
        );
        assert_eq!(params.subject_alt_names.len(), 1);
        assert!(matches!(params.is_ca, rcgen::IsCa::ExplicitNoCa));
    }

    fn identity_files(directory: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .expect("folder")
            .map(|entry| entry.expect("entry").file_name().into_string().expect("name"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn lan_tls_creates_a_private_p256_identity_and_a_lan_listener() {
        let (_root, config_path, data) = hub_fixture();
        let report = setup_lan_tls(&config_path, HOST, SystemTime::now()).expect("set up");
        assert_eq!(report.status, "created");
        assert_eq!(report.public_url, "https://office-mac.local:8443/");
        assert_eq!(report.bind, "0.0.0.0:8443");
        assert!(report.certificate_expires_at_ms.is_some());
        let text = serde_json::to_string(&report).expect("report JSON");
        assert!(!text.contains("PRIVATE"));

        let directory = data.join("tls");
        assert_eq!(mode(&directory), 0o700);
        let config = HubConfig::load(&config_path).expect("new config loads");
        assert_eq!(config.bind.to_string(), "0.0.0.0:8443");
        let tls = config.tls.as_ref().expect("[tls]");
        let certificate = tls.certificate_path.clone();
        let private_key = tls.private_key_path.clone();
        assert_eq!(certificate.parent(), Some(directory.as_path()));
        assert_eq!(private_key.parent(), Some(directory.as_path()));
        // Exactly the run's two files remain: the pending marker is retired.
        let names = identity_files(&directory);
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().all(|name| name.starts_with("lan-tls-") && name.ends_with(".pem")));
        assert_eq!(mode(&certificate), 0o600);
        assert_eq!(mode(&private_key), 0o600);
        let key_pem = fs::read_to_string(&private_key).expect("key");
        let key = rcgen::KeyPair::from_pem(&key_pem).expect("PEM key");
        assert_eq!(key.algorithm(), &rcgen::PKCS_ECDSA_P256_SHA256);
        assert_eq!(tls.public_url, "https://office-mac.local:8443/");
        tesatlas_hub::server::validate_tls_identity(tls).expect("Serve admits the identity");
        assert_eq!(config.collector.interval_seconds, 60, "other settings are kept");
        assert_eq!(mode(&config_path), 0o600);

        // A second run finds [tls] and writes nothing.
        let before = fs::read(&config_path).expect("config bytes");
        let key_before = fs::read(&private_key).expect("key bytes");
        let again = setup_lan_tls(&config_path, HOST, SystemTime::now()).expect("existing");
        assert_eq!(again.status, "existing");
        assert_eq!(again.public_url, "https://office-mac.local:8443/");
        assert_eq!(fs::read(&config_path).expect("config bytes"), before);
        assert_eq!(fs::read(&private_key).expect("key bytes"), key_before);
    }

    #[test]
    fn lan_tls_refusals_leave_the_configuration_and_folder_unchanged() {
        let (_root, config_path, data) = hub_fixture();
        let before = fs::read(&config_path).expect("config bytes");

        let error = setup_lan_tls(&config_path, "Office-Mac.local", SystemTime::now())
            .expect_err("uppercase host refused");
        assert!(error.to_string().contains(".local"));
        assert_eq!(fs::read(&config_path).expect("config bytes"), before);
        assert!(!data.join("tls").exists());

        // A folder open to others is refused before anything is generated.
        let directory = data.join("tls");
        fs::create_dir(&directory).expect("tls folder");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).expect("open folder");
        setup_lan_tls(&config_path, HOST, SystemTime::now()).expect_err("open folder refused");
        assert_eq!(fs::read(&config_path).expect("config bytes"), before);
        assert!(fs::read_dir(&directory).expect("folder").next().is_none());

        // A link in place of the configuration is not followed.
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).expect("private");
        let link = config_path.with_file_name("linked.toml");
        std::os::unix::fs::symlink(&config_path, &link).expect("link");
        setup_lan_tls(&link, HOST, SystemTime::now()).expect_err("linked config refused");
        assert!(fs::read_dir(&directory).expect("folder").next().is_none());
    }

    #[test]
    fn lan_tls_refuses_a_configuration_serve_would_refuse() {
        let (_root, config_path, data) = hub_fixture();
        let before = fs::read(&config_path).expect("config bytes");
        // Group-writable: Serve refuses it, so lan-tls never rewrites it as 0600.
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o620)).expect("open config");
        let error = setup_lan_tls(&config_path, HOST, SystemTime::now())
            .expect_err("group-writable config refused");
        assert!(error.to_string().contains("private file"), "{error}");
        assert_eq!(fs::read(&config_path).expect("config bytes"), before);
        assert_eq!(mode(&config_path), 0o620);
        assert!(!data.join("tls").exists());

        // A FIFO at the configuration path is refused without blocking on open.
        let fifo = config_path.with_file_name("fifo.toml");
        let made = std::process::Command::new("/usr/bin/mkfifo")
            .arg("-m")
            .arg("600")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(made.success());
        setup_lan_tls(&fifo, HOST, SystemTime::now()).expect_err("FIFO refused");
        assert!(!data.join("tls").exists());
    }

    #[test]
    fn lan_tls_never_overwrites_a_concurrent_configuration_change() {
        let (_root, config_path, data) = hub_fixture();
        let edited = format!(
            "{}\n[backup]\nenabled = false\n",
            fs::read_to_string(&config_path).expect("config")
        );
        let error = super::super::setup_lan_tls_observed(
            &config_path,
            HOST,
            SystemTime::now(),
            &mut || {
                // Another writer replaces the file while the identity is prepared.
                let other = config_path.with_file_name("other.toml");
                fs::write(&other, &edited).expect("other");
                fs::set_permissions(&other, fs::Permissions::from_mode(0o600)).expect("private");
                fs::rename(&other, &config_path).expect("replace");
            },
            Duration::from_secs(30),
        )
        .expect_err("conflict reported");
        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(fs::read_to_string(&config_path).expect("config"), edited);
        // The run removed its own identity files and marker.
        assert!(identity_files(&data.join("tls")).is_empty());
        let folder = identity_files(config_path.parent().expect("folder"));
        assert!(folder.iter().all(|name| !name.ends_with(".tmp")), "{folder:?}");
    }

    #[test]
    fn lan_tls_overlapping_setups_end_with_a_valid_referenced_identity() {
        use std::sync::mpsc;
        let (_root, config_path, data) = hub_fixture();
        let (reached_tx, reached_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let setup = |host: &'static str, mut hold: Box<dyn FnMut() + Send>| {
            let path = config_path.clone();
            std::thread::spawn(move || {
                super::super::setup_lan_tls_observed(&path, host, SystemTime::now(), &mut *hold, Duration::from_secs(30))
                    .map_err(|error| error.to_string())
            })
        };
        // The first setup is held after its identity and staged configuration are validated.
        let first = setup(HOST, Box::new(move || {
            reached_tx.send(()).expect("reached");
            release_rx.recv().expect("released");
        }));
        reached_rx.recv_timeout(Duration::from_secs(30)).expect("first setup held");
        let second = setup("other-mac.local", Box::new(|| {}));
        // The second neither reaps the held run's marker, key and certificate nor publishes.
        std::thread::sleep(Duration::from_millis(400));
        assert!(!second.is_finished(), "the second setup waits for the writer lock");
        assert_eq!(identity_files(&data.join("tls")).len(), 3);
        release_tx.send(()).expect("release");
        let first = first.join().expect("first thread").expect("first setup");
        let second = second.join().expect("second thread").expect("second setup");
        assert_eq!((first.status, second.status), ("created", "existing"));
        assert_eq!(second.public_url, first.public_url);
        let tls = HubConfig::load(&config_path).expect("config").tls.expect("tls");
        super::super::server::validate_tls_identity(&tls).expect("a valid referenced key and certificate pair");
        assert_eq!(identity_files(&data.join("tls")).len(), 2, "only the published key and certificate");

        // A setup that cannot take the lock in time changes nothing.
        let held = super::super::lock_config_writer(&config_path, Duration::ZERO).expect("lock");
        let before = fs::read(&config_path).expect("config");
        let error = super::super::setup_lan_tls_observed(&config_path, HOST, SystemTime::now(), &mut || {}, Duration::from_millis(200))
            .expect_err("refused while held");
        assert!(error.to_string().contains("still running"), "{error}");
        assert_eq!(fs::read(&config_path).expect("config"), before);
        drop(held);
    }

    #[test]
    fn lan_tls_settles_an_interrupted_run_and_never_adopts_a_stray_file() {
        let (_root, config_path, data) = hub_fixture();
        let directory = data.join("tls");
        fs::create_dir(&directory).expect("tls folder");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).expect("private");
        // A run lost before its configuration was published: marker, key, partial certificate.
        let lost = "0123456789abcdef0123456789abcdef";
        fs::write(directory.join(format!("lan-tls-{lost}.pending")), "").expect("marker");
        fs::write(directory.join(format!("lan-tls-{lost}-private-key.pem")), "lost key").expect("key");
        fs::write(directory.join(format!("lan-tls-{lost}-certificate.pem")), "par").expect("cert");
        // A file no run of ours claimed.
        fs::write(directory.join("private-key.pem"), "stray").expect("stray");

        let report = setup_lan_tls(&config_path, HOST, SystemTime::now()).expect("retry works");
        assert_eq!(report.status, "created");
        let names = identity_files(&directory);
        assert!(!names.iter().any(|name| name.contains(lost)), "{names:?}");
        assert!(!names.iter().any(|name| name.ends_with(".pending")), "{names:?}");
        assert_eq!(fs::read_to_string(directory.join("private-key.pem")).expect("stray"), "stray");
        let config = HubConfig::load(&config_path).expect("config");
        let tls = config.tls.expect("[tls]");
        assert_ne!(tls.private_key_path, directory.join("private-key.pem"));

        // A run lost after its configuration was published keeps its files; only the
        // marker goes.
        let published = tls
            .private_key_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("lan-tls-"))
            .and_then(|name| name.strip_suffix("-private-key.pem"))
            .expect("run id")
            .to_owned();
        fs::write(directory.join(format!("lan-tls-{published}.pending")), "").expect("marker");
        let again = setup_lan_tls(&config_path, HOST, SystemTime::now()).expect("existing");
        assert_eq!(again.status, "existing");
        assert!(tls.private_key_path.exists());
        assert!(tls.certificate_path.exists());
        assert!(!directory.join(format!("lan-tls-{published}.pending")).exists());
    }

    #[test]
    fn serving_answer_names_only_this_process_and_configuration() {
        use super::super::{serving_answer, ServingAnswer};
        let digest = "a".repeat(64);
        let ready = br#"{"status":"ready"}"#;
        let idle = br#"{"status":"not_ready","reason":"collector_absent"}"#;
        assert_eq!(
            serving_answer(200, Some("42"), Some(&digest), ready, 42, &digest),
            Some(ServingAnswer::Serving("ready"))
        );
        // Between windows the Hub still serves.
        assert_eq!(
            serving_answer(503, Some("42"), Some(&digest), idle, 42, &digest),
            Some(ServingAnswer::Serving("between_windows"))
        );
        // Another process, another configuration or no headers is not this Hub serving.
        assert_eq!(serving_answer(200, Some("43"), Some(&digest), ready, 42, &digest), None);
        assert_eq!(serving_answer(200, Some("42"), Some(&"b".repeat(64)), ready, 42, &digest), None);
        assert_eq!(serving_answer(200, None, None, ready, 42, &digest), None);
        assert_eq!(serving_answer(500, Some("42"), Some(&digest), ready, 42, &digest), None);
        assert_eq!(serving_answer(200, Some("42"), Some(&digest), idle, 42, &digest), None);
        assert_eq!(serving_answer(200, Some("42"), Some(&digest), b"not json", 42, &digest), None);
    }

    #[test]
    fn serving_answer_refuses_every_readiness_failure_but_a_closed_window() {
        use super::super::{serving_answer, ServingAnswer};
        let digest = "a".repeat(64);
        let answer = |status, body: &str| serving_answer(status, Some("42"), Some(&digest), body.as_bytes(), 42, &digest);
        for reason in ["credential_store_error", "catalogue_unavailable", "lifecycle_quarantined",
                       "published_content_unservable", "collector_stale", "collector_auth_terminal"] {
            let body = format!(r#"{{"status":"not_ready","reason":"{reason}"}}"#);
            assert_eq!(answer(503, &body), Some(ServingAnswer::Refused(reason.to_owned())), "{reason}");
        }
        // No reason, or one that is not a redacted code, is still a refusal.
        for body in [r#"{"status":"not_ready"}"#, r#"{"status":"not_ready","reason":"/private/path"}"#] {
            assert_eq!(answer(503, body), Some(ServingAnswer::Refused("unknown".to_owned())), "{body}");
        }
        assert_eq!(answer(200, r#"{"status":"ready","reason":"collector_absent"}"#), None);
    }

    #[test]
    fn serving_check_waits_between_one_and_150_seconds() {
        let parse = |wait: &str| {
            super::super::Cli::try_parse_from(["tesatlas-hub", "serving-check", "--wait-seconds", wait])
        };
        for wait in ["1", "45", "150"] {
            let cli = parse(wait).expect("accepted wait");
            assert!(matches!(
                cli.command,
                super::super::Command::ServingCheck { wait_seconds } if wait_seconds.to_string() == wait
            ));
        }
        for wait in ["0", "151", "-1"] {
            assert!(parse(wait).is_err(), "{wait}");
        }
        let default = super::super::Cli::try_parse_from(["tesatlas-hub", "serving-check"]).expect("default");
        assert!(matches!(default.command, super::super::Command::ServingCheck { wait_seconds: 30 }));
    }

    #[test]
    fn launchctl_pid_is_read_only_from_a_running_job() {
        use super::super::launchctl_running_pid;
        assert_eq!(launchctl_running_pid("\tstate = running\n\tpid = 812\n"), Some(812));
        assert_eq!(launchctl_running_pid("\tstate = waiting\n\tpid = 812\n"), None);
        assert_eq!(launchctl_running_pid("\tstate = running\n"), None);
        assert_eq!(launchctl_running_pid("\tstate = running\n\tpid = 0\n"), None);
    }
}

#[path = "n1_cli_report_tests.rs"]
mod n1_cli_report_tests;

#[cfg(target_os = "macos")]
fn v2_credential_pair_fixture(owner: bool) -> (tempfile::TempDir, HubStore) {
    use tesatlas_hub::auth::credential_sign_in as credentials;
    let root = tempfile::tempdir().expect("temporary Hub");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).expect("private Hub");
    let store = HubStore::initialize(root.path()).expect("store");
    if owner { credentials::seed_owner_pair_for_test(root.path(), "access-0001", "refresh-0001", None).expect("Owner entry"); }
    let fleet = super::FleetSetupCredentials::new("access-0002".into(), "refresh-0002".into(), "client-0001".into(), super::FleetRegion::EuropeMiddleEastAndAfrica, 3600).expect("Fleet fixture");
    // The Hub's own Fleet grant, the only Fleet pair the vault admits (M09-T11).
    credentials::seed_own_fleet_pair_for_test(root.path(), &fleet).expect("Fleet entry");
    (root, store)
}

#[path = "service_cli_tests.rs"]
mod service_cli_tests;
