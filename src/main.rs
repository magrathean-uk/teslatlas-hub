use std::{
    fs,
    io::{Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::Engine;
use clap::{Parser, Subcommand};
use qrcode::{QrCode, render::unicode::Dense1x2};
use sha2::{Digest, Sha256};
use teslatlas_hub::{
    collector,
    config::HubConfig,
    credentials::{CredentialDirectory, OwnerTokens},
    db::{HubStore, NoWakeVerificationError, ObservationVerificationError},
    server,
    setup::{self, SetupOptions},
    teslamate_direct::preflight_teslamate_import,
    teslamate_import::{
        TeslaMateImportRequest, TeslaMateImportScope, import_all_from_postgres,
        import_from_postgres, derive_effective_import_profile,
    },
};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const MAX_STDIN_CREDENTIAL_BYTES: usize = 32 * 1024;
const LEGACY_OWNER_API_BASE: &str = "https://owner-api.teslamotors.com";
const HUB_API_SERVICE: &str = "teslatlas-hub.service";
const HUB_SUPERVISED_SERVICE: &str = "teslatlas-hub-supervised.service";

fn no_wake_command_error(error: NoWakeVerificationError) -> Box<dyn std::error::Error> {
    Box::new(error)
}

#[derive(Debug, Parser)]
#[command(
    name = "teslatlas-hub",
    version,
    about = "Native Teslatlas Hub service"
)]
struct Cli {
    #[arg(long, global = true, default_value = "/etc/teslatlas/config.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create or reuse the protected LAN identity and start Hub.
    Setup {
        /// Reachable LAN IP advertised to the phone. Auto-detected when omitted.
        #[arg(long)]
        lan_address: Option<IpAddr>,
        /// Direct TLS listener port.
        #[arg(long, default_value_t = 8443)]
        port: u16,
        /// Write and validate setup without changing systemd state.
        #[arg(long)]
        no_start: bool,
    },
    /// Initialize or migrate the local Hub database.
    Init,
    /// Validate the database and print a machine-readable health report.
    Doctor,
    /// Capture the current durable observation watermark for one source car.
    #[command(name = "observation-watermark")]
    ObservationWatermark {
        /// Source/import car id selected for the cutover.
        #[arg(long)]
        car_id: i64,
    },
    /// Verify that a strictly newer observation is durably committed.
    #[command(name = "verify-observation")]
    VerifyObservation {
        /// Source/import car id selected for the cutover.
        #[arg(long)]
        car_id: i64,
        /// Observation id returned by observation-watermark.
        #[arg(long)]
        watermark: i64,
    },
    /// Capture the current durable outbound-request audit watermark.
    #[command(name = "audit-watermark")]
    AuditWatermark,
    /// Verify a correlation-scoped no-wake audit window. An empty window is
    /// deliberately not proof until outbound clients write request receipts.
    #[command(name = "verify-no-wake")]
    VerifyNoWake {
        /// Receipt id returned by audit-watermark before the proof window.
        #[arg(long)]
        audit_watermark: i64,
        /// Unique collection-run identifier used to isolate request receipts.
        #[arg(long)]
        correlation_id: Uuid,
        /// Optional source/import car id when pairing audit proof with durable observation proof.
        #[arg(long, requires = "observation_watermark")]
        car_id: Option<i64>,
        /// Optional observation id returned by observation-watermark.
        #[arg(long, requires = "car_id")]
        observation_watermark: Option<i64>,
    },
    /// Start the Hub HTTP service.
    Serve,
    /// Perform one explicit, no-wake compatibility collection through systemd.
    CollectOnce,
    /// Run the opt-in supervised no-wake collector loop through systemd.
    CollectSupervised,
    /// Read and publish one full TeslaMate history snapshot through systemd.
    ImportTeslaMate {
        /// TeslaMate local car ID. Omit to import all discovered cars in order.
        #[arg(long)]
        car_id: Option<i64>,
    },
    /// Inspect TeslaMate import readiness without opening or writing Hub state.
    #[command(name = "preflight-tesla-mate")]
    PreflightTeslaMate {
        /// TeslaMate local car ID to inspect.
        #[arg(long)]
        car_id: i64,
    },
    /// Transfer the one encrypted TeslaMate legacy token pair into a
    /// host-encrypted Hub credential. This command is unit-only and never
    /// writes or controls TeslaMate.
    #[command(hide = true)]
    ImportTeslaMateOwnerTokens,
    /// Internal privileged helper. Reads one bounded strict token-pair JSON
    /// from stdin and never prints credential material.
    #[command(hide = true)]
    StoreTeslaMateOwnerTokens,
    /// Display one short-lived, single-use device pairing QR.
    #[command(alias = "create-pairing")]
    Pair {
        /// Human label shown only to the Hub owner, such as "Bolyki iPhone".
        #[arg(long, default_value = "Teslatlas iPhone")]
        label: String,
        /// Lifetime before the invitation becomes unusable.
        #[arg(long, default_value_t = 900)]
        expires_in_seconds: u64,
        /// Print the secret-bearing machine-readable payload for explicit debugging.
        #[arg(long, hide = true)]
        json: bool,
    },
    /// Validate database integrity, report quarantined sessions, and clean orphaned packs.
    Repair,
    /// Create a consistent catalogue and immutable-pack backup generation.
    Backup {
        /// New directory that will contain the complete backup generation.
        #[arg(long)]
        destination: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("teslatlas-hub: {error}");
            let mut source = error.source();
            while let Some(cause) = source {
                eprintln!("caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    if let Command::Setup {
        lan_address,
        port,
        no_start,
    } = cli.command
    {
        let report = setup::configure(&SetupOptions {
            config_path: cli.config,
            lan_address,
            port,
        })?;
        if !no_start {
            protect_service_identity(&report)?;
            systemctl(&["daemon-reload"])?;
            systemctl(&["enable", "teslatlas-hub.service"])?;
            systemctl(&["restart", "teslatlas-hub.service"])?;
            wait_for_service(&report).await?;
        }
        println!(
            "{}",
            serde_json::json!({
                "status": if report.created { "created" } else { "ready" },
                "endpoint": report.public_url,
                "certificatePath": report.certificate_path,
                "privateKeyPath": report.private_key_path,
                "serviceStarted": !no_start,
            })
        );
        return Ok(());
    }

    if matches!(&cli.command, Command::ImportTeslaMateOwnerTokens) {
        let config = HubConfig::load(&cli.config)?;
        import_teslamate_owner_tokens(&config).await?;
        println!("{{\"status\":\"owner_tokens_migrated\"}}");
        return Ok(());
    }

    if matches!(&cli.command, Command::StoreTeslaMateOwnerTokens) {
        require_root()?;
        let mut bytes = Vec::with_capacity(MAX_STDIN_CREDENTIAL_BYTES);
        std::io::stdin()
            .take((MAX_STDIN_CREDENTIAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_STDIN_CREDENTIAL_BYTES {
            return Err("credential input is too large".into());
        }
        let tokens = OwnerTokens::from_credential_json(&bytes)?;
        activate_legacy_tokens(&tokens, &cli.config)?;
        println!("{{\"status\":\"owner_tokens_activated\"}}");
        return Ok(());
    }

    if let Command::PreflightTeslaMate { car_id } = &cli.command {
        let config = HubConfig::load(&cli.config)?;
        let import_config = config.teslamate.import_config()?;
        let credentials = CredentialDirectory::required_from_systemd_environment()?;
        let password = credentials.teslamate_postgres_password()?;
        let target_packs_dir = config.data_dir.join("packs");
        match preflight_teslamate_import(
            &import_config.source,
            &password,
            *car_id,
            import_config.limits,
            &target_packs_dir,
        )
        .await
        {
            Ok(report) => println!("{}", serde_json::to_string(&report)?),
            Err(error) => println!(
                "{}",
                serde_json::json!({
                    "selectedCarId": car_id,
                    "admission": {"passed": false, "reason": "preflight_error"},
                    "error": error.to_string(),
                })
            ),
        }
        return Ok(());
    }

    match &cli.command {
        Command::ObservationWatermark { car_id } => {
            let config = HubConfig::load(&cli.config)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let watermark = match store.observation_watermark(*car_id) {
                Ok(watermark) => watermark,
                Err(error) => return Err(observation_command_error(
                    "observation-watermark",
                    *car_id,
                    error,
                )),
            };
            println!(
                "{}",
                serde_json::json!({
                    "status": "captured",
                    "command": "observation-watermark",
                    "carId": watermark.source_car_id,
                    "sourceId": watermark.source_id,
                    "vehicleId": watermark.vehicle_id,
                    "watermark": watermark.observation_id,
                    "observedAtMs": watermark.observed_at_ms,
                    "receivedAtMs": watermark.received_at_ms,
                })
            );
            return Ok(());
        }
        Command::VerifyObservation { car_id, watermark } => {
            let config = HubConfig::load(&cli.config)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let verification = match store.verify_observation_after(*car_id, *watermark) {
                Ok(verification) => verification,
                Err(error) => return Err(observation_command_error(
                    "verify-observation",
                    *car_id,
                    error,
                )),
            };
            let verified = verification.verified();
            println!(
                "{}",
                serde_json::json!({
                    "status": if verified { "verified" } else { "not_verified" },
                    "command": "verify-observation",
                    "verified": verified,
                    "carId": verification.source_car_id,
                    "sourceId": verification.source_id,
                    "vehicleId": verification.vehicle_id,
                    "afterWatermark": verification.after_observation_id,
                    "latestObservationId": verification.latest_observation_id,
                    "latestObservedAtMs": verification.latest_observed_at_ms,
                    "latestReceivedAtMs": verification.latest_received_at_ms,
                })
            );
            if !verified {
                return Err("no strictly newer durable observation".into());
            }
            return Ok(());
        }
        Command::AuditWatermark => {
            let config = HubConfig::load(&cli.config)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let watermark = store.outbound_request_watermark()?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "captured",
                    "command": "audit-watermark",
                    "watermark": watermark.receipt_id,
                })
            );
            return Ok(());
        }
        Command::VerifyNoWake {
            audit_watermark,
            correlation_id,
            car_id,
            observation_watermark,
        } => {
            let observation = match (car_id, observation_watermark) {
                (Some(car_id), Some(watermark)) => Some((*car_id, *watermark)),
                (None, None) => None,
                _ => return Err("--car-id and --observation-watermark must be supplied together".into()),
            };
            let config = HubConfig::load(&cli.config)?;
            let store = HubStore::open_read_only(&config.data_dir)?;
            let verification = store
                .verify_no_wake_after(*audit_watermark, *correlation_id, observation)
                .map_err(no_wake_command_error)?;
            let verified = verification.verified();
            let observation = verification.observation.as_ref();
            println!(
                "{}",
                serde_json::json!({
                    "status": if verified { "verified" } else { "not_verified" },
                    "command": "verify-no-wake",
                    "verified": verified,
                    "auditVerified": verification.audit_verified(),
                    "auditWatermark": verification.after_receipt_id,
                    "correlationId": verification.correlation_id,
                    "matchingReceipts": verification.matching_receipts,
                    "unresolvedReceipts": verification.unresolved_receipts,
                    "unresolvedStreamSessions": verification.unresolved_stream_sessions,
                    "directWakeReceipts": verification.direct_wake_receipts,
                    "conditionalWithoutPowerReceipts": verification.conditional_without_power_receipts,
                    "observationVerified": observation.map(|value| value.verified()),
                    "carId": observation.map(|value| value.source_car_id),
                    "observationWatermark": observation.map(|value| value.after_observation_id),
                    "latestObservationId": observation.and_then(|value| value.latest_observation_id),
                })
            );
            if !verified {
                return Err("no-wake proof did not satisfy all fail-closed checks".into());
            }
            return Ok(());
        }
        _ => {}
    }

    let config = HubConfig::load(&cli.config)?;
    let store = HubStore::initialize(&config.data_dir)?;
    match cli.command {
        Command::Setup { .. } => unreachable!("setup returns before loading Hub state"),
        Command::Init => {
            println!("initialized {}", store.database_path().display());
        }
        Command::Doctor => {
            store.catalogue_check()?;
            println!(
                "{{\"status\":\"ok\",\"version\":\"{}\",\"sqlite\":\"{}\",\"database\":\"{}\"}}",
                teslatlas_hub::BUILD_VERSION,
                store.sqlite_version()?,
                store.database_path().display()
            );
        }
        Command::ObservationWatermark { .. }
        | Command::VerifyObservation { .. }
        | Command::AuditWatermark
        | Command::VerifyNoWake { .. } => {
            unreachable!("read-only observation commands return before opening Hub state")
        }
        Command::Serve => server::serve(store, &config).await?,
        Command::CollectOnce => {
            let report = collector::collect_once_from_systemd(&store, &config).await?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::CollectSupervised => {
            collector::run_supervised_from_systemd(&store, &config).await?;
        }
        Command::ImportTeslaMate { car_id } => {
            let import_config = config.teslamate.import_config()?;
            let profile = derive_effective_import_profile(
                import_config.limits.parallel_copy_lanes,
                &import_config.performance_profile,
                &config.data_dir,
            )?;
            tracing::info!(
                performance_profile_version = profile.version,
                performance_profile_enabled = import_config.performance_profile.enabled,
                configured_parallel_copy_lanes = import_config.limits.parallel_copy_lanes,
                selected_parallel_copy_lanes = profile.parallel_copy_lanes,
                cpu_parallelism = ?profile.capabilities.available_parallelism.map(|value| value.get()),
                filesystem_free_bytes = ?profile.capabilities.filesystem_free_bytes,
                filesystem_free_inodes = ?profile.capabilities.filesystem_free_inodes,
                selection_reason = profile.reason.as_str(),
                "TeslaMate import performance profile selected"
            );
            let mut import_limits = import_config.limits;
            import_limits.parallel_copy_lanes = profile.parallel_copy_lanes;
            let credentials = CredentialDirectory::required_from_systemd_environment()?;
            let password = credentials.teslamate_postgres_password()?;
            let cursor_key = credentials.cursor_key()?;
            match car_id {
                Some(car_id) => {
                    let report = import_from_postgres(
                        &store,
                        &import_config.source,
                        &password,
                        &cursor_key,
                        &TeslaMateImportRequest {
                            source_key: import_config.source_key,
                            scope: TeslaMateImportScope::Selected(car_id),
                            imported_at_ms: current_epoch_ms()?,
                        },
                        import_limits,
                    )
                    .await?;
                    println!(
                        "{}",
                        serde_json::json!({
                            "scope": "selected",
                            "sourceId": report.source_id,
                            "vehicleId": report.vehicle_id,
                            "snapshotId": report.snapshot_id,
                            "sequence": report.sequence,
                            "projectedRows": report.projected_rows,
                            "skipped": report.skipped,
                        })
                    );
                }
                None => {
                    let summary = import_all_from_postgres(
                        &store,
                        &import_config.source,
                        &password,
                        &cursor_key,
                        &import_config.source_key,
                        current_epoch_ms()?,
                        import_limits,
                    )
                    .await?;
                    println!("{}", serde_json::to_string(&summary)?);
                    if summary.has_failures() {
                        return Err("one or more TeslaMate cars failed to import".into());
                    }
                }
            }
        }
        Command::PreflightTeslaMate { .. } => {
            unreachable!("preflight returns before opening Hub storage")
        }
        Command::ImportTeslaMateOwnerTokens => {
            unreachable!("token migration returns before opening Hub storage")
        }
        Command::StoreTeslaMateOwnerTokens => {
            unreachable!("credential storage returns before opening Hub storage")
        }
        Command::Pair {
            label,
            expires_in_seconds,
            json,
        } => {
            let tls = config
                .tls
                .as_ref()
                .ok_or("device pairing requires configured TLS")?;
            if expires_in_seconds == 0 {
                return Err("pairing expiry must be greater than zero".into());
            }
            let created_at_ms = current_epoch_ms()?;
            let ttl_ms = expires_in_seconds
                .checked_mul(1_000)
                .ok_or("pairing expiry is too large")?;
            let ttl_ms = i64::try_from(ttl_ms).map_err(|_| "pairing expiry is too large")?;
            let expires_at_ms = created_at_ms
                .checked_add(ttl_ms)
                .ok_or("pairing expiry is too large")?;
            let invitation = store.create_pairing(&label, created_at_ms, expires_at_ms)?;
            let tls_pin = leaf_certificate_sha256(&tls.certificate_path)?;
            let pairing_uri = pairing_uri(
                &tls.public_url,
                &tls_pin,
                invitation.pairing_id,
                invitation.secret(),
            );
            if json {
                // Explicit debug mode only. The Hub never stores the raw
                // pairing secret or writes it to service logs.
                println!(
                    "{}",
                    serde_json::json!({
                        "pairingId": invitation.pairing_id,
                        "secret": invitation.secret(),
                        "expiresAtMs": invitation.expires_at_ms,
                        "endpoint": tls.public_url,
                        "tlsPin": tls_pin,
                        "pairingUri": pairing_uri,
                    })
                );
            } else {
                let qr = render_pairing_qr(&pairing_uri)?;
                println!("Scan with Teslatlas:\n{qr}");
                println!("Expires in {expires_in_seconds} seconds.");
            }
        }
        Command::Repair => {
            let report = store.repair()?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::Backup { destination } => {
            store.backup_to(&destination)?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "ok",
                    "destination": destination,
                })
            );
        }
    }
    Ok(())
}

fn observation_command_error(
    command: &str,
    car_id: i64,
    error: ObservationVerificationError,
) -> Box<dyn std::error::Error> {
    println!(
        "{}",
        serde_json::json!({
            "status": "error",
            "command": command,
            "carId": car_id,
            "errorCode": error.code(),
        })
    );
    Box::new(std::io::Error::other(error.to_string()))
}

async fn import_teslamate_owner_tokens(
    config: &HubConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let import_config = config.teslamate.import_config()?;
    let credentials = CredentialDirectory::required_from_systemd_environment()?;
    let password = credentials.teslamate_postgres_password()?;
    let encryption_key = credentials.teslamate_encryption_key()?;
    let ciphertexts = teslatlas_hub::teslamate_reader::read_legacy_token_ciphertexts(
        &import_config.source,
        &password,
        import_config.limits,
    )
    .await?;
    let tokens = teslatlas_hub::teslamate_token::decrypt_legacy_owner_tokens(
        encryption_key.as_bytes(),
        &ciphertexts.access,
        &ciphertexts.refresh,
    )?;
    credentials.persist_linux_teslamate_owner_tokens(&tokens)?;
    Ok(())
}

fn require_root() -> Result<(), Box<dyn std::error::Error>> {
    let identity = ProcessCommand::new("id").arg("-u").output()?;
    if !identity.status.success() || identity.stdout.trim_ascii() != b"0" {
        return Err("credential activation requires root".into());
    }
    Ok(())
}

fn activate_legacy_tokens(
    tokens: &OwnerTokens,
    config_path: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let config_before = fs::read(config_path)?;
    let config_after = merge_legacy_config(&config_before)?;
    let parsed: HubConfig = toml::from_str(std::str::from_utf8(&config_after)?)?;
    parsed.validate()?;

    let api_state = service_state(HUB_API_SERVICE)?;
    let supervised_state = service_state(HUB_SUPERVISED_SERVICE)?;

    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let credentials = CredentialDirectory::required_from_systemd_environment()?;
        credentials.persist_linux_teslamate_owner_tokens(tokens)?;
        write_atomic_file(config_path, &config_after, 0o644)?;
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", HUB_API_SERVICE])?;
        systemctl(&["restart", HUB_API_SERVICE])?;
        systemctl(&["enable", "--now", HUB_SUPERVISED_SERVICE])?;
        Ok(())
    })();

    if let Err(error) = result {
        let _ = systemctl(&["daemon-reload"]);
        let _ = restore_service_state(HUB_API_SERVICE, api_state);
        let _ = restore_service_state(HUB_SUPERVISED_SERVICE, supervised_state);
        return Err(error);
    }
    Ok(())
}

fn merge_legacy_config(source: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut root = std::str::from_utf8(source)?.parse::<toml::Table>()?;
    let collector = root
        .entry("collector")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or("collector must be a TOML table")?;
    collector.insert(
        "owner_api_base_url".to_owned(),
        toml::Value::String(LEGACY_OWNER_API_BASE.to_owned()),
    );
    collector.insert("interval_seconds".to_owned(), toml::Value::Integer(1));
    let legacy_auth = collector
        .entry("legacy_auth".to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or("collector.legacy_auth must be a TOML table")?;
    legacy_auth.insert("enabled".to_owned(), toml::Value::Boolean(true));
    Ok(toml::to_string_pretty(&root)?.into_bytes())
}


fn write_atomic_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn protect_service_identity(report: &setup::SetupResult) -> Result<(), std::io::Error> {
    let identity_directory = report
        .certificate_path
        .parent()
        .ok_or_else(|| std::io::Error::other("TLS identity has no parent directory"))?;
    let tls_directory = identity_directory
        .parent()
        .ok_or_else(|| std::io::Error::other("TLS root has no parent directory"))?;
    run_process(
        "chown",
        &[
            std::ffi::OsStr::new("root:teslatlas"),
            tls_directory.as_os_str(),
            identity_directory.as_os_str(),
            report.certificate_path.as_os_str(),
            report.private_key_path.as_os_str(),
        ],
    )
}

fn systemctl(arguments: &[&str]) -> Result<(), std::io::Error> {
    let status = ProcessCommand::new("systemctl").args(arguments).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "systemctl {} failed with {status}",
            arguments.join(" ")
        )))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ServiceState {
    enabled: bool,
    active: bool,
}

fn service_state(unit: &str) -> Result<ServiceState, std::io::Error> {
    let enabled = ProcessCommand::new("systemctl")
        .args(["is-enabled", "--quiet", unit])
        .status()?
        .success();
    let active = ProcessCommand::new("systemctl")
        .args(["is-active", "--quiet", unit])
        .status()?
        .success();
    Ok(ServiceState { enabled, active })
}

fn restore_service_state(unit: &str, state: ServiceState) -> Result<(), std::io::Error> {
    if state.enabled {
        systemctl(&["enable", unit])?;
    } else {
        systemctl(&["disable", unit])?;
    }
    if state.active {
        systemctl(&["start", unit])?;
    } else {
        systemctl(&["stop", unit])?;
    }
    Ok(())
}

async fn wait_for_service(report: &setup::SetupResult) -> Result<(), std::io::Error> {
    teslatlas_hub::crypto::install_default_provider();
    let certificate_pem = fs::read(&report.certificate_path)?;
    let certificate = reqwest::Certificate::from_pem(&certificate_pem)
        .map_err(|error| std::io::Error::other(format!("cannot load Hub certificate: {error}")))?;
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate(certificate)
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| std::io::Error::other(format!("cannot build health client: {error}")))?;
    let ready_url = format!("{}/readyz", report.public_url.trim_end_matches('/'));
    for _ in 0..30 {
        let active = ProcessCommand::new("systemctl")
            .args(["is-active", "--quiet", "teslatlas-hub.service"])
            .status()?;
        if active.success()
            && client
                .get(&ready_url)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(std::io::Error::other(
        "teslatlas-hub.service did not become ready over TLS",
    ))
}

fn run_process(program: &str, arguments: &[&std::ffi::OsStr]) -> Result<(), std::io::Error> {
    let status = ProcessCommand::new(program).args(arguments).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{program} failed with {status}"
        )))
    }
}

fn render_pairing_qr(pairing_uri: &str) -> Result<String, qrcode::types::QrError> {
    Ok(QrCode::new(pairing_uri.as_bytes())?
        .render::<Dense1x2>()
        .quiet_zone(true)
        .build())
}

/// SHA-256 pin for the first PEM certificate (the server leaf) in the active
/// TLS chain. Pairing carries this non-secret value so an iPhone can pin the
/// exact identity before it sends its single-use claim secret.
fn leaf_certificate_sha256(certificate_path: &std::path::Path) -> Result<String, std::io::Error> {
    let pem = fs::read_to_string(certificate_path)?;
    let begin = "-----BEGIN CERTIFICATE-----";
    let end = "-----END CERTIFICATE-----";
    let Some(after_begin) = pem.split_once(begin).map(|(_, rest)| rest) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "TLS certificate has no PEM leaf",
        ));
    };
    let Some((encoded, _)) = after_begin.split_once(end) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "TLS certificate leaf PEM is incomplete",
        ));
    };
    let encoded: String = encoded
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "TLS leaf PEM is invalid")
        })?;
    if der.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "TLS leaf PEM is empty",
        ));
    }
    Ok(hex::encode(Sha256::digest(der)))
}

fn pairing_uri(endpoint: &str, tls_pin: &str, pairing_id: uuid::Uuid, secret: &str) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("endpoint", endpoint);
    query.append_pair("pairing_id", &pairing_id.to_string());
    query.append_pair("secret", secret);
    query.append_pair("tls_pin", tls_pin);
    format!("teslatlas-hub://pair?{}", query.finish())
}

fn current_epoch_ms() -> Result<i64, std::io::Error> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| std::io::Error::other("system clock exceeds epoch milliseconds"))
}

#[cfg(test)]
mod tests {
    use std::{io::Write, path::PathBuf};

    use base64::Engine;
    use clap::Parser;
    use sha2::Digest;

    use super::{
        Cli, Command, LEGACY_OWNER_API_BASE, ServiceState, dropin_content,
        leaf_certificate_sha256, merge_legacy_config, pairing_uri, render_pairing_qr,
    };
    use uuid::Uuid;

    #[test]
    fn preflight_command_parses_a_required_car_id() {
        let cli = Cli::try_parse_from([
            "teslatlas-hub",
            "preflight-tesla-mate",
            "--car-id",
            "17",
        ])
        .expect("preflight CLI");
        assert!(matches!(cli.command, Command::PreflightTeslaMate { car_id: 17 }));
    }

    #[test]
    fn observation_commands_parse_their_machine_readable_inputs() {
        let watermark = Cli::try_parse_from([
            "teslatlas-hub",
            "observation-watermark",
            "--car-id",
            "17",
        ])
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
    fn pairing_uri_encodes_its_endpoint_as_one_query_value() {
        let pin = "a".repeat(64);
        let uri = pairing_uri(
            "https://hub.example/",
            &pin,
            Uuid::nil(),
            "0123456789abcdef",
        );
        assert!(uri.contains("endpoint=https%3A%2F%2Fhub.example%2F"));
        assert!(uri.contains("pairing_id=00000000-0000-0000-0000-000000000000"));
        assert!(uri.contains(&format!("tls_pin={pin}")));
    }

    #[test]
    fn leaf_certificate_pin_hashes_first_pem_certificate() {
        let leaf = [1_u8, 2, 3, 4, 5];
        let temporary = tempfile::NamedTempFile::new().expect("temporary PEM");
        let mut pem = temporary.reopen().expect("reopen PEM");
        writeln!(
            pem,
            "ignored\n-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----",
            base64::engine::general_purpose::STANDARD.encode(leaf),
            base64::engine::general_purpose::STANDARD.encode([9_u8, 8, 7])
        )
        .expect("write PEM");
        pem.flush().expect("flush PEM");

        assert_eq!(
            leaf_certificate_sha256(temporary.path()).expect("leaf pin"),
            hex::encode(sha2::Sha256::digest(leaf))
        );
    }

    #[test]
    fn pairing_qr_renders_without_printing_the_raw_secret() {
        let uri = pairing_uri(
            "https://192.168.1.10:8443",
            &"a".repeat(64),
            Uuid::nil(),
            "0123456789abcdef",
        );
        let qr = render_pairing_qr(&uri).expect("render QR");
        assert!(qr.contains('█') || qr.contains('▀') || qr.contains('▄'));
        assert!(!qr.contains("0123456789abcdef"));
    }

    #[test]
    fn legacy_activation_merges_settings_without_duplicate_keys() {
        let source = br#"data_dir = "/var/lib/teslatlas"
bind = "127.0.0.1:8443"
[collector]
owner_api_base_url = "https://old.example"
interval_seconds = 30
[collector.legacy_auth]
enabled = false
"#;
        let merged = String::from_utf8(merge_legacy_config(source).expect("merged config"))
            .expect("UTF-8 config");
        let parsed: toml::Table = merged.parse().expect("valid TOML");
        let collector = parsed["collector"].as_table().expect("collector table");
        assert_eq!(
            collector["owner_api_base_url"].as_str(),
            Some(LEGACY_OWNER_API_BASE)
        );
        assert_eq!(collector["interval_seconds"].as_integer(), Some(1));
        assert_eq!(collector["legacy_auth"]["enabled"].as_bool(), Some(true));
        assert_eq!(merged.matches("owner_api_base_url").count(), 1);
        assert_eq!(merged.matches("interval_seconds").count(), 1);
        assert!(!merged.contains("old.example"));
    }

    #[test]
    fn legacy_dropin_contains_only_credential_paths() {
        let dropin = String::from_utf8(dropin_content(
            "teslamate-owner-tokens",
            PathBuf::from("/etc/teslatlas/credentials/teslamate-owner-tokens").as_ref(),
        ))
        .expect("drop-in text");
        assert_eq!(
            dropin,
            "[Service]\nLoadCredentialEncrypted=teslamate-owner-tokens:/etc/teslatlas/credentials/teslamate-owner-tokens\n"
        );
        assert!(!dropin.contains("access_token"));
        assert!(!dropin.contains("refresh_token"));
    }

    #[test]
    fn inactive_before_activation_is_enabled_and_started() {
        let before = ServiceState {
            enabled: false,
            active: false,
        };
        let after = ServiceState {
            enabled: true,
            active: true,
        };
        assert!(!before.enabled && !before.active);
        assert!(after.enabled && after.active);
    }

    #[test]
    fn failed_supervised_start_restores_previous_unit_state() {
        let before = ServiceState {
            enabled: false,
            active: false,
        };
        let failed = ServiceState {
            enabled: true,
            active: false,
        };
        assert_ne!(failed, before);
        assert_eq!(
            before,
            ServiceState {
                enabled: false,
                active: false
            }
        );
    }
}
