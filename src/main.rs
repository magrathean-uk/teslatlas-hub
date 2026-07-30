use std::{
    fs,
    net::IpAddr,
    path::PathBuf,
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
    credentials::CredentialDirectory,
    db::HubStore,
    server,
    setup::{self, SetupOptions},
    teslamate_import::{TeslaMateImportRequest, import_from_postgres},
};
use tracing_subscriber::EnvFilter;

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
    /// Start the Hub HTTP service.
    Serve,
    /// Perform one explicit, no-wake compatibility collection through systemd.
    CollectOnce,
    /// Run the opt-in supervised no-wake collector loop through systemd.
    CollectSupervised,
    /// Read and publish one full TeslaMate history snapshot through systemd.
    ImportTeslaMate {
        /// TeslaMate local car ID selected for this migration source.
        #[arg(long)]
        car_id: i64,
    },
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
    /// Validate database integrity, clear quarantined sessions, and clean orphaned packs.
    Repair,
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

    let config = HubConfig::load(&cli.config)?;
    let store = HubStore::initialize(&config.data_dir)?;
    match cli.command {
        Command::Setup { .. } => unreachable!("setup returns before loading Hub state"),
        Command::Init => {
            println!("initialized {}", store.database_path().display());
        }
        Command::Doctor => {
            store.quick_check()?;
            println!(
                "{{\"status\":\"ok\",\"version\":\"{}\",\"sqlite\":\"{}\",\"database\":\"{}\"}}",
                teslatlas_hub::BUILD_VERSION,
                store.sqlite_version()?,
                store.database_path().display()
            );
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
            let credentials = CredentialDirectory::required_from_systemd_environment()?;
            let password = credentials.teslamate_postgres_password()?;
            let cursor_key = credentials.cursor_key()?;
            let report = import_from_postgres(
                &store,
                &import_config.source,
                &password,
                &cursor_key,
                &TeslaMateImportRequest {
                    source_key: import_config.source_key,
                    selected_car_id: car_id,
                    imported_at_ms: current_epoch_ms()?,
                },
                import_config.limits,
            )
            .await?;
            println!(
                "{}",
                serde_json::json!({
                    "sourceId": report.source_id,
                    "vehicleId": report.vehicle_id,
                    "snapshotId": report.snapshot_id,
                    "sequence": report.sequence,
                    "projectedRows": report.projected_rows,
                })
            );
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
    }
    Ok(())
}

fn protect_service_identity(report: &setup::SetupResult) -> Result<(), std::io::Error> {
    let identity_directory = report
        .certificate_path
        .parent()
        .ok_or_else(|| std::io::Error::other("TLS identity has no parent directory"))?;
    run_process(
        "chown",
        &[
            std::ffi::OsStr::new("root:teslatlas"),
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
    use std::io::Write;

    use base64::Engine;
    use sha2::Digest;

    use super::{leaf_certificate_sha256, pairing_uri, render_pairing_qr};
    use uuid::Uuid;

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
}
