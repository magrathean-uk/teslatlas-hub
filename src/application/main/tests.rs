// SPDX-License-Identifier: AGPL-3.0-only

#[cfg(target_os = "macos")]
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

use clap::Parser;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use sha2::Digest;
#[cfg(target_os = "macos")]
use sha2::Sha256;

use super::{
    Cli, Command, ControlCommand, MAX_TLS_CERTIFICATE_CHAIN_BYTES, MAX_TLS_PRIVATE_KEY_BYTES,
    PairingCommandError, PairingCommandInput, execute_pairing_at, leaf_certificate_sha256,
    leaf_certificate_sha256_after_open, pairing_uri, persist_and_present_pairing,
    read_tls_identity_file, render_pairing_qr, run, run_immutable_diagnostic_with,
};
#[cfg(target_os = "macos")]
use super::{
    MAC_COMMAND_PROXY_RETRY_DELAY, MAX_MIGRATION_ENCRYPTION_KEY_BYTES,
    MAX_MIGRATION_POSTGRES_PASSWORD_BYTES, MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES,
    MAX_MIGRATION_TOKEN_BYTES, MAX_MIGRATION_TOKEN_FILE_BYTES, MacCommandProxySpec,
    MacServeControl, MacServeWorkerStopTimeout, MigrationSecretReadError, ServiceCommand,
    clear_provider_credentials, command_requires_user_hub_admission, decode_setup_fleet_stdin,
    migration_start_requested, migration_stop_confirmed, persist_fleet_setup_and_drop_legacy,
    persist_legacy_setup_and_drop_fleet, persist_migrated_legacy_tokens,
    read_migration_encryption_key, read_migration_postgres_password, read_migration_secret,
    read_migration_secret_file_with_hooks, run_macos_serve_supervisor,
    teslamate_check_failure_details, teslamate_version_confirmation,
    validate_legacy_setup_provider, validate_streaming_setting,
};
use teslatlas_hub::db::HubStore;
#[cfg(target_os = "macos")]
use teslatlas_hub::protocol::{
    CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V3, OpaqueCursor, PROTOCOL_V1,
};
#[cfg(target_os = "macos")]
use teslatlas_hub::{
    teslamate_reader::TeslaMateReaderError, teslamate_schema::SchemaCompatibilityError,
};
use uuid::Uuid;

#[test]
fn legal_aliases_and_source_command_parse_without_configuration() {
    for name in ["legal", "licence", "license"] {
        let cli = Cli::try_parse_from(["teslatlas-hub", name]).expect("legal CLI alias");
        assert!(matches!(cli.command, Command::Legal));
    }
    let source = Cli::try_parse_from(["teslatlas-hub", "source"]).expect("source CLI");
    assert!(matches!(source.command, Command::Source));
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
            "/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy",
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
fn teslamate_check_failures_are_redacted_and_actionable() {
    let older = TeslaMateReaderError::Schema(SchemaCompatibilityError::LegacyMigration {
        found: 1,
        minimum: 2,
    });
    let newer = TeslaMateReaderError::Schema(SchemaCompatibilityError::UnreviewedMigration {
        found: 3,
        maximum: 2,
    });
    let selected = TeslaMateReaderError::SelectedCarMissing { selected_car_id: 7 };
    let ambiguous = TeslaMateReaderError::AmbiguousOpenSession {
        drives: 2,
        charges: 1,
        states: 1,
    };

    assert_eq!(
        teslamate_check_failure_details(&older).1,
        "older_than_v4_2_compatible_schema"
    );
    assert_eq!(teslamate_check_failure_details(&older).2, Some(1));
    assert!(teslamate_check_failure_details(&older).3.contains("update"));
    assert_eq!(
        teslamate_check_failure_details(&newer).1,
        "newer_than_v4_2_compatible_schema"
    );
    assert!(
        teslamate_check_failure_details(&newer)
            .3
            .contains("Do not downgrade")
    );
    assert_eq!(
        teslamate_check_failure_details(&selected).1,
        "selected_car_missing"
    );
    assert!(!teslamate_check_failure_details(&selected).3.contains('7'));
    assert_eq!(
        teslamate_check_failure_details(&ambiguous).1,
        "ambiguous_open_session"
    );
    assert!(!teslamate_check_failure_details(&ambiguous).3.contains("2"));
}

#[cfg(target_os = "macos")]
#[test]
fn teslamate_check_requires_honest_version_confirmation() {
    let unconfirmed = teslamate_version_confirmation(false);
    assert_eq!(unconfirmed.0, "confirmation_required");
    assert_eq!(unconfirmed.1, "v4_2_version_unconfirmed");
    assert!(unconfirmed.2.contains("cannot prove"));

    let confirmed = teslamate_version_confirmation(true);
    assert_eq!(confirmed.0, "compatible");
    assert_eq!(confirmed.1, "v4_2_compatible_schema");
    assert!(!confirmed.1.contains("exact"));
}

#[cfg(target_os = "macos")]
#[test]
fn onboarding_migration_cli_is_explicit_and_noninteractive() {
    let check = Cli::try_parse_from([
        "teslatlas-hub",
        "teslamate-check",
        "--source",
        "postgresql://reader@localhost/teslamate",
        "--car-id",
        "7",
        "--postgres-password-file",
        "password",
        "--acknowledge-v4-2-compatible-schema",
    ])
    .expect("compatibility-check CLI");
    assert!(matches!(
        check.command,
        Command::TeslaMateCheck { car_id: 7, .. }
    ));
    assert!(!command_requires_user_hub_admission(&check.command));

    assert!(
        Cli::try_parse_from([
            "teslatlas-hub",
            "migrate",
            "--source",
            "postgresql://reader@localhost/teslamate",
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
        "teslatlas-hub",
        "migrate",
        "--source",
        "postgresql://reader@localhost/teslamate",
        "--car-id",
        "7",
        "--postgres-password-file",
        "password",
        "--encryption-key-file",
        "key",
        "--online-snapshot",
        "--acknowledge-v4-2-compatible-schema",
    ])
    .expect("online migration CLI");
    assert!(matches!(
        migration.command,
        Command::Migrate {
            online_snapshot: true,
            acknowledge_v4_2_compatible_schema: true,
            ..
        }
    ));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn teslamate_check_invalid_source_does_not_create_hub_target() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = temporary.path().join("absent/config.toml");
    let cli = Cli::try_parse_from([
        "teslatlas-hub",
        "--config",
        config.to_str().expect("UTF-8 config path"),
        "teslamate-check",
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
            source: "not-a-postgres-url".to_owned(),
            car_id: 7,
            postgres_password_file: PathBuf::from("unused-password"),
            encryption_key_file: Some(PathBuf::from("unused-key")),
            access_token_file: None,
            refresh_token_file: None,
            online_snapshot: true,
            acknowledge_v4_2_compatible_schema: false,
        },
    };

    let error = run(cli)
        .await
        .expect_err("missing version confirmation must fail before migration");
    assert!(error.to_string().contains("requires --acknowledge"));
    assert!(!config.exists());
    assert!(!config.parent().expect("config parent").exists());
}

#[cfg(unix)]
#[test]
fn bootstrap_and_explicit_vehicle_commands_parse() {
    let bootstrap = Cli::try_parse_from(["teslatlas-hub", "bootstrap"]).expect("bootstrap CLI");
    assert!(matches!(bootstrap.command, Command::Bootstrap));
    assert!(Cli::try_parse_from(["teslatlas-hub", "control", "wake"]).is_err());
    let wake = Cli::try_parse_from([
        "teslatlas-hub",
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
        Cli::try_parse_from(["teslatlas-hub", "control", "climate-start", "--confirm"]).is_ok()
    );
}

#[test]
fn paired_device_controls_parse_without_vehicle_selection() {
    let list = Cli::try_parse_from(["teslatlas-hub", "control", "paired-devices"])
        .expect("paired-device list CLI");
    assert!(matches!(
        list.command,
        Command::Control {
            command: ControlCommand::PairedDevices,
            ..
        }
    ));
    let revoke = Cli::try_parse_from([
        "teslatlas-hub",
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
        let mut value = b"teslamate-key".to_vec();
        value.extend_from_slice(ending);
        fs::write(&path, value).expect("key file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("key mode");
        assert_eq!(
            read_migration_encryption_key(&path)
                .expect("line ending is not part of the key")
                .as_slice(),
            b"teslamate-key"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn migration_password_and_token_semantic_boundaries_include_line_endings() {
    use teslatlas_hub::teslamate_token::{
        CLOAK_ENVELOPE_OVERHEAD_BYTES, MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES,
        encrypt_legacy_owner_token_files,
    };

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
    assert_eq!(
        MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES,
        MAX_MIGRATION_TOKEN_BYTES + CLOAK_ENVELOPE_OVERHEAD_BYTES
    );
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
    let (access, refresh) = encrypt_legacy_owner_token_files(
        b"boundary-test-key",
        read_migration_secret(&access_path, MAX_MIGRATION_TOKEN_FILE_BYTES)
            .expect("bounded access token"),
        read_migration_secret(&refresh_path, MAX_MIGRATION_TOKEN_FILE_BYTES)
            .expect("bounded refresh token"),
    )
    .expect("semantic token cap");
    assert_eq!(access.len(), MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES);
    assert_eq!(refresh.len(), MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES);

    fs::write(&access_path, vec![b'a'; MAX_MIGRATION_TOKEN_BYTES + 1])
        .expect("oversized access token");
    fs::set_permissions(&access_path, fs::Permissions::from_mode(0o600)).expect("token mode");
    assert!(
        encrypt_legacy_owner_token_files(
            b"boundary-test-key",
            read_migration_secret(&access_path, MAX_MIGRATION_TOKEN_FILE_BYTES)
                .expect("raw bounded access token"),
            read_migration_secret(&refresh_path, MAX_MIGRATION_TOKEN_FILE_BYTES)
                .expect("bounded refresh token"),
        )
        .is_err()
    );
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
    "TESLATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_CHILD";
#[cfg(target_os = "macos")]
const MACOS_SUPERVISOR_SIGTERM_CHILD_BIND_ENV: &str =
    "TESLATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_BIND";
#[cfg(target_os = "macos")]
const MACOS_SUPERVISOR_SIGTERM_CHILD_RECEIPT_ENV: &str =
    "TESLATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_RECEIPT";
#[cfg(target_os = "macos")]
const MACOS_SUPERVISOR_SIGTERM_CHILD_FIXTURE_ENV: &str =
    "TESLATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_FIXTURE";
#[cfg(target_os = "macos")]
const MACOS_SUPERVISOR_SIGTERM_CHILD_RUN_ENV: &str =
    "TESLATLAS_HUB_TEST_MACOS_SUPERVISOR_SIGTERM_RUN";
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
    use teslatlas_hub::{
        credentials::OwnerTokens, db::TeslaMateLegacyTokenStore, fleet_api::FleetRegion,
        fleet_credentials::FleetSetupCredentials, teslamate_credentials::random_encryption_key,
        teslamate_token::encrypt_legacy_owner_tokens,
    };

    let temporary = tempfile::tempdir().expect("temporary Hub");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
        .expect("private data directory");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let fleet = FleetSetupCredentials::new(
        "fleet-access".to_owned(),
        "fleet-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        3_600,
    )
    .expect("Fleet credentials");
    persist_fleet_setup_and_drop_legacy(
        temporary.path(),
        &store,
        &fleet,
        std::time::SystemTime::now(),
    )
    .expect("seed Fleet");
    let legacy = OwnerTokens::from_file_bytes(
        zeroize::Zeroizing::new(b"doctor-access".to_vec()),
        zeroize::Zeroizing::new(b"doctor-refresh".to_vec()),
    )
    .expect("legacy credentials");
    let encryption_key = random_encryption_key().expect("random encryption key");
    let (access, refresh) = encrypt_legacy_owner_tokens(&encryption_key, &legacy).expect("encrypt");
    let stored = TeslaMateLegacyTokenStore::imported(access, refresh).expect("legacy store");
    persist_migrated_legacy_tokens(temporary.path(), &store, &encryption_key, &stored)
        .expect("copy TeslaMate tokens without deleting Fleet");

    let config_path = temporary.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "data_dir = {:?}\nbind = '127.0.0.1:18443'\n\n[collector]\ninterval_seconds = 0\n",
            temporary.path()
        ),
    )
    .expect("write config");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).expect("private config");

    run(Cli {
        config: Some(config_path),
        command: Command::Doctor,
    })
    .await
    .expect("doctor succeeds on a healthy catalogue");

    assert!(
        store
            .load_teslamate_legacy_tokens()
            .expect("legacy after doctor")
            .is_some(),
        "doctor must not delete Owner tokens"
    );
    assert!(
        store
            .load_fleet_tokens()
            .expect("Fleet after doctor")
            .is_some(),
        "doctor must not delete Fleet tokens"
    );
    let inventory = store.catalogue_inventory().expect("inventory");
    assert_eq!(inventory.journal_mode, "wal");
    assert_eq!(inventory.teslamate_legacy_token_rows, 1);
    assert_eq!(inventory.fleet_token_rows, 1);
}

#[test]
fn immutable_diagnostic_waits_for_a_transient_wal() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
        .expect("protect temporary directory");
    HubStore::initialize(temporary.path()).expect("store initializes");
    let wal = temporary.path().join("hub.sqlite-wal");
    fs::write(&wal, b"pending").expect("create pending WAL witness");
    let mut waits = 0;

    run_immutable_diagnostic_with(
        temporary.path(),
        |store| {
            store.catalogue_check()?;
            Ok(())
        },
        || {
            waits += 1;
            if wal.exists() {
                fs::remove_file(&wal).expect("settle WAL");
            }
        },
    )
    .expect("diagnostic opens after WAL settles");

    assert_eq!(waits, 1);
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
        Cli::try_parse_from(["teslatlas-hub", "observation-watermark", "--car-id", "17"])
            .expect("watermark CLI");
    assert!(matches!(
        watermark.command,
        Command::ObservationWatermark { car_id: 17 }
    ));

    let verify = Cli::try_parse_from([
        "teslatlas-hub",
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
        "teslatlas-hub",
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
        Cli::try_parse_from(["teslatlas-hub", "control", "export-gpx", "--drive-id", "0",])
            .is_err()
    );

    let sign_out =
        Cli::try_parse_from(["teslatlas-hub", "control", "sign-out"]).expect("sign-out CLI");
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
    let cli = Cli::try_parse_from(["teslatlas-hub", "observe", "--duration-seconds", "3600"])
        .expect("observe CLI");
    assert!(matches!(
        cli.command,
        Command::Observe {
            duration_seconds: 3600
        }
    ));
    assert!(Cli::try_parse_from(["teslatlas-hub", "observe", "--duration-seconds", "0",]).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn setup_command_accepts_private_token_files_and_positive_optional_vehicle() {
    let cli = Cli::try_parse_from([
        "teslatlas-hub",
        "setup",
        "--access-token-file",
        "access",
        "--refresh-token-file",
        "refresh",
        "--vehicle-id",
        "70",
    ])
    .expect("setup CLI");
    assert!(matches!(
        cli.command,
        Command::Setup {
            access_token_file: Some(access_token_file),
            refresh_token_file: Some(refresh_token_file),
            tokens_stdin: false,
            vehicle_id: Some(70),
            all_vehicles: false,
        } if access_token_file.as_path() == Path::new("access")
            && refresh_token_file.as_path() == Path::new("refresh")
    ));
    assert!(
        Cli::try_parse_from([
            "teslatlas-hub",
            "setup",
            "--access-token-file",
            "access",
            "--refresh-token-file",
            "refresh",
            "--vehicle-id",
            "0",
        ])
        .is_err()
    );
    assert!(Cli::try_parse_from(["teslatlas-hub", "setup"]).is_err());
    assert!(
        Cli::try_parse_from(["teslatlas-hub", "setup", "--access-token-file", "access",]).is_err()
    );
    let stdin =
        Cli::try_parse_from(["teslatlas-hub", "setup", "--tokens-stdin"]).expect("stdin setup CLI");
    assert!(matches!(
        stdin.command,
        Command::Setup {
            access_token_file: None,
            refresh_token_file: None,
            tokens_stdin: true,
            vehicle_id: None,
            all_vehicles: false,
        }
    ));
    assert!(
        Cli::try_parse_from([
            "teslatlas-hub",
            "setup",
            "--tokens-stdin",
            "--all-vehicles",
            "--vehicle-id",
            "70",
        ])
        .is_err()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn fleet_setup_is_stdin_only_and_decodes_bounded_fields() {
    let selected = Cli::try_parse_from(["teslatlas-hub", "setup-fleet", "--vehicle-id", "70"])
        .expect("Fleet setup CLI");
    assert!(matches!(
        selected.command,
        Command::SetupFleet {
            vehicle_id: Some(70),
            all_vehicles: false,
        }
    ));
    let all = Cli::try_parse_from(["teslatlas-hub", "setup-fleet", "--all-vehicles"])
        .expect("all Fleet vehicles CLI");
    assert!(matches!(
        all.command,
        Command::SetupFleet {
            vehicle_id: None,
            all_vehicles: true,
        }
    ));
    assert!(
        Cli::try_parse_from([
            "teslatlas-hub",
            "setup-fleet",
            "--all-vehicles",
            "--vehicle-id",
            "70",
        ])
        .is_err()
    );
    assert!(
        decode_setup_fleet_stdin(
            br#"{"accessToken":"e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iLCJ2ZWhpY2xlX2NtZHMiLCJ2ZWhpY2xlX2NoYXJnaW5nX2NtZHMiXX0.sig","refreshToken":"refresh","clientId":"client","region":"europe_middle_east_and_africa","expiresInSeconds":3600}"#,
        )
        .is_ok()
    );
    assert!(
        decode_setup_fleet_stdin(
            br#"{"accessToken":"e30.eyJzY3AiOlsidmVoaWNsZV9kZXZpY2VfZGF0YSJdfQ.sig","refreshToken":"refresh","clientId":"client","region":"europe_middle_east_and_africa","expiresInSeconds":3600}"#,
        )
        .is_err()
    );
    assert!(
        decode_setup_fleet_stdin(
            br#"{"accessToken":"access","refreshToken":"refresh","clientId":"client","region":"eu","expiresInSeconds":3600}"#,
        )
        .is_err()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn fleet_rejects_enabling_legacy_streaming() {
    use teslatlas_hub::config::CollectorProvider;

    assert!(validate_streaming_setting(CollectorProvider::Fleet, Some(true)).is_err());
    assert!(validate_streaming_setting(CollectorProvider::Fleet, Some(false)).is_ok());
    assert!(validate_streaming_setting(CollectorProvider::Legacy, Some(true)).is_ok());
}

#[cfg(target_os = "macos")]
#[test]
fn legacy_setup_requires_legacy_provider_before_mutation() {
    use teslatlas_hub::config::CollectorProvider;

    assert!(validate_legacy_setup_provider(CollectorProvider::Legacy).is_ok());
    assert!(validate_legacy_setup_provider(CollectorProvider::Fleet).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn sign_out_clears_both_providers_but_preserves_cursor_key() {
    use teslatlas_hub::{
        credentials::OwnerTokens,
        db::TeslaMateLegacyTokenStore,
        fleet_api::FleetRegion,
        fleet_credentials::{FleetSetupCredentials, persist_fleet_setup_credentials},
        teslamate_credentials::{
            load_or_create_cursor_key, random_encryption_key, replace_key_and_tokens,
        },
        teslamate_token::encrypt_legacy_owner_tokens,
    };

    let temporary = tempfile::tempdir().expect("temporary Hub");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
        .expect("private temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let cursor_before = load_or_create_cursor_key(temporary.path()).expect("cursor key");
    let cursor_proof = supervisor_cursor_proof(&cursor_before);
    let legacy = OwnerTokens::from_file_bytes(
        zeroize::Zeroizing::new(b"access".to_vec()),
        zeroize::Zeroizing::new(b"refresh".to_vec()),
    )
    .expect("legacy credentials");
    let legacy_key = random_encryption_key().expect("random legacy key");
    let (access, refresh) =
        encrypt_legacy_owner_tokens(&legacy_key, &legacy).expect("encrypt legacy");
    let legacy_store = TeslaMateLegacyTokenStore::imported(access, refresh).expect("legacy store");
    replace_key_and_tokens(temporary.path(), &store, &legacy_key, &legacy_store)
        .expect("persist legacy");
    let fleet = FleetSetupCredentials::new(
        "fleet-access".to_owned(),
        "fleet-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        3_600,
    )
    .expect("Fleet credentials");
    persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &fleet,
        std::time::SystemTime::now(),
    )
    .expect("persist Fleet");
    assert!(teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path()).exists());

    clear_provider_credentials(temporary.path(), &store).expect("sign out");

    assert!(
        store
            .load_teslamate_legacy_tokens()
            .expect("legacy row")
            .is_none()
    );
    assert!(store.load_fleet_tokens().expect("Fleet row").is_none());
    assert!(!teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path()).exists());
    let cursor_after = load_or_create_cursor_key(temporary.path()).expect("cursor remains");
    assert_eq!(supervisor_cursor_proof(&cursor_after), cursor_proof);
}

#[cfg(target_os = "macos")]
#[test]
fn sign_out_attempts_legacy_removal_after_fleet_key_failure() {
    use teslatlas_hub::{
        credentials::OwnerTokens,
        db::TeslaMateLegacyTokenStore,
        teslamate_credentials::{random_encryption_key, replace_key_and_tokens},
        teslamate_token::encrypt_legacy_owner_tokens,
    };

    let temporary = tempfile::tempdir().expect("temporary Hub");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
        .expect("private temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let legacy = OwnerTokens::from_file_bytes(
        zeroize::Zeroizing::new(b"access".to_vec()),
        zeroize::Zeroizing::new(b"refresh".to_vec()),
    )
    .expect("legacy credentials");
    let legacy_key = random_encryption_key().expect("random legacy key");
    let (access, refresh) =
        encrypt_legacy_owner_tokens(&legacy_key, &legacy).expect("encrypt legacy");
    let legacy_store = TeslaMateLegacyTokenStore::imported(access, refresh).expect("legacy store");
    replace_key_and_tokens(temporary.path(), &store, &legacy_key, &legacy_store)
        .expect("persist legacy");

    let invalid_fleet_key = teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path());
    fs::create_dir(&invalid_fleet_key).expect("invalid Fleet key directory");
    fs::set_permissions(&invalid_fleet_key, fs::Permissions::from_mode(0o700))
        .expect("private invalid Fleet key");

    let error = clear_provider_credentials(temporary.path(), &store)
        .expect_err("Fleet key failure must be reported");
    assert!(error.to_string().contains("Fleet credentials"));
    assert!(
        store
            .load_teslamate_legacy_tokens()
            .expect("legacy row")
            .is_none(),
        "Legacy credentials must still be removed"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn setup_clears_the_other_provider_token_generation() {
    use teslatlas_hub::{
        credentials::OwnerTokens,
        db::TeslaMateLegacyTokenStore,
        fleet_api::FleetRegion,
        fleet_credentials::FleetSetupCredentials,
        teslamate_credentials::{random_encryption_key, replace_key_and_tokens},
        teslamate_token::encrypt_legacy_owner_tokens,
    };

    let temporary = tempfile::tempdir().expect("temporary Hub");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
        .expect("private temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let legacy = OwnerTokens::from_file_bytes(
        zeroize::Zeroizing::new(b"access".to_vec()),
        zeroize::Zeroizing::new(b"refresh".to_vec()),
    )
    .expect("legacy credentials");
    let legacy_key = random_encryption_key().expect("random legacy key");
    let (access, refresh) =
        encrypt_legacy_owner_tokens(&legacy_key, &legacy).expect("encrypt legacy");
    let legacy_store = TeslaMateLegacyTokenStore::imported(access, refresh).expect("legacy store");
    replace_key_and_tokens(temporary.path(), &store, &legacy_key, &legacy_store)
        .expect("persist legacy");
    let fleet = FleetSetupCredentials::new(
        "fleet-access".to_owned(),
        "fleet-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        3_600,
    )
    .expect("Fleet credentials");
    persist_fleet_setup_and_drop_legacy(
        temporary.path(),
        &store,
        &fleet,
        std::time::SystemTime::now(),
    )
    .expect("setup-fleet drops legacy");
    assert!(
        store
            .load_teslamate_legacy_tokens()
            .expect("legacy row")
            .is_none()
    );
    assert!(store.load_fleet_tokens().expect("Fleet row").is_some());
    assert!(teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path()).exists());

    persist_legacy_setup_and_drop_fleet(temporary.path(), &store, &legacy)
        .expect("setup drops Fleet");
    assert!(
        store
            .load_teslamate_legacy_tokens()
            .expect("legacy row")
            .is_some()
    );
    assert!(store.load_fleet_tokens().expect("Fleet row").is_none());
    assert!(!teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path()).exists());
}

#[cfg(target_os = "macos")]
#[test]
fn migrate_copies_legacy_tokens_without_deleting_fleet() {
    use teslatlas_hub::{
        credentials::OwnerTokens, db::TeslaMateLegacyTokenStore, fleet_api::FleetRegion,
        fleet_credentials::FleetSetupCredentials, teslamate_credentials::random_encryption_key,
        teslamate_token::encrypt_legacy_owner_tokens,
    };

    let temporary = tempfile::tempdir().expect("temporary Hub");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
        .expect("private temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let fleet = FleetSetupCredentials::new(
        "fleet-access".to_owned(),
        "fleet-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        3_600,
    )
    .expect("Fleet credentials");
    persist_fleet_setup_and_drop_legacy(
        temporary.path(),
        &store,
        &fleet,
        std::time::SystemTime::now(),
    )
    .expect("seed Fleet");
    assert!(store.load_fleet_tokens().expect("Fleet row").is_some());

    let legacy = OwnerTokens::from_file_bytes(
        zeroize::Zeroizing::new(b"migrate-access".to_vec()),
        zeroize::Zeroizing::new(b"migrate-refresh".to_vec()),
    )
    .expect("legacy credentials");
    let encryption_key = random_encryption_key().expect("random encryption key");
    let (access, refresh) =
        encrypt_legacy_owner_tokens(&encryption_key, &legacy).expect("encrypt legacy");
    let stored = TeslaMateLegacyTokenStore::imported(access, refresh).expect("legacy store");
    persist_migrated_legacy_tokens(temporary.path(), &store, &encryption_key, &stored)
        .expect("migrate copies TeslaMate tokens");
    assert!(
        store
            .load_teslamate_legacy_tokens()
            .expect("legacy row")
            .is_some()
    );
    assert!(
        store.load_fleet_tokens().expect("Fleet row").is_some(),
        "TeslaMate import must not delete Fleet credentials"
    );
    assert!(teslatlas_hub::fleet_credentials::fleet_key_path(temporary.path()).exists());
}

#[cfg(target_os = "macos")]
#[test]
fn service_commands_parse_without_live_store_admission() {
    for (name, expected) in [
        ("status", "status"),
        ("start", "start"),
        ("stop", "stop"),
        ("restart", "restart"),
    ] {
        let cli = Cli::try_parse_from(["teslatlas-hub", "service", name]).expect("service CLI");
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
    assert!(command_requires_user_hub_admission(&Command::Init));
    assert!(command_requires_user_hub_admission(&Command::Setup {
        access_token_file: Some(PathBuf::from("access")),
        refresh_token_file: Some(PathBuf::from("refresh")),
        tokens_stdin: false,
        vehicle_id: None,
        all_vehicles: false,
    }));
    assert!(command_requires_user_hub_admission(&Command::SetupFleet {
        vehicle_id: None,
        all_vehicles: true,
    }));
    assert!(command_requires_user_hub_admission(&Command::Serve));
    assert!(command_requires_user_hub_admission(&Command::Observe {
        duration_seconds: 1,
    }));
    assert!(command_requires_user_hub_admission(&Command::Migrate {
        source: "postgresql://localhost/teslamate".to_owned(),
        car_id: 1,
        postgres_password_file: PathBuf::from("password"),
        encryption_key_file: Some(PathBuf::from("key")),
        access_token_file: None,
        refresh_token_file: None,
        online_snapshot: false,
        acknowledge_v4_2_compatible_schema: true,
    }));
    assert!(command_requires_user_hub_admission(&Command::Pair {
        label: "test phone".to_owned(),
        expires_in_seconds: 900,
        json: false,
    }));
    assert!(command_requires_user_hub_admission(&Command::Repair));
    assert!(command_requires_user_hub_admission(&Command::Backup {
        destination: PathBuf::from("backup"),
    }));
    assert!(command_requires_user_hub_admission(
        &Command::ExportRecoveryCredentials {
            destination: PathBuf::from("credentials.tthcr"),
            recovery_key_file: PathBuf::from("recovery.key"),
        }
    ));
    assert!(command_requires_user_hub_admission(
        &Command::RestoreRecoveryCredentials {
            source: PathBuf::from("credentials.tthcr"),
            recovery_key_file: PathBuf::from("recovery.key"),
        }
    ));
    assert!(!command_requires_user_hub_admission(&Command::Doctor));
    assert!(!command_requires_user_hub_admission(&Command::Legal));
    assert!(!command_requires_user_hub_admission(&Command::Source));
    assert!(!command_requires_user_hub_admission(&Command::Status));
    assert!(!command_requires_user_hub_admission(&Command::Preflight));
    assert!(!command_requires_user_hub_admission(&Command::Service {
        command: ServiceCommand::Status,
    }));
    assert!(!command_requires_user_hub_admission(&Command::Control {
        vehicle_id: None,
        command: ControlCommand::Pause,
    }));
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
            Err(teslatlas_hub::db::StoreError::PairingRejected)
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
        || Err(teslatlas_hub::db::StoreError::PairingRejected),
        || Err(teslatlas_hub::db::StoreError::PairingRejected),
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
        || Err(teslatlas_hub::db::StoreError::PairingRejected),
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
fn pairing_qr_renders_without_printing_the_raw_secret() {
    let uri = pairing_uri(
        "https://192.168.1.10:8443",
        &"a".repeat(64),
        Uuid::nil(),
        "0123456789abcdef",
    )
    .expect("pairing URI");
    let qr = render_pairing_qr(&uri).expect("render QR");
    assert!(qr.contains('█') || qr.contains('▀') || qr.contains('▄'));
    assert!(!qr.contains("0123456789abcdef"));
}
