use std::{
    fs,
    path::PathBuf,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::Engine;
use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use teslatlas_hub::{
    collector,
    config::HubConfig,
    credentials::CredentialDirectory,
    db::HubStore,
    server,
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
    /// Initialize or migrate the local Hub database.
    Init,
    /// Validate the database and print a machine-readable health report.
    Doctor,
    /// Start the Hub HTTP service.
    Serve,
    /// Perform one explicit, no-wake compatibility collection through systemd.
    CollectOnce,
    /// Read and publish one full TeslaMate history snapshot through systemd.
    ImportTeslaMate {
        /// TeslaMate local car ID selected for this migration source.
        #[arg(long)]
        car_id: i64,
    },
    /// Make one short-lived, single-use device pairing invitation.
    CreatePairing {
        /// Human label shown only to the Hub owner, such as "Bolyki iPhone".
        #[arg(long)]
        label: String,
        /// Lifetime before the invitation becomes unusable.
        #[arg(long, default_value_t = 300)]
        expires_in_seconds: u64,
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
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let config = HubConfig::load(&cli.config)?;
    let store = HubStore::initialize(&config.data_dir)?;
    match cli.command {
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
        Command::CreatePairing {
            label,
            expires_in_seconds,
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
            // This is intentionally a local, one-time console result. The
            // Hub never stores the raw pairing secret or writes it to logs.
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
        }
    }
    Ok(())
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

    use super::{leaf_certificate_sha256, pairing_uri};
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
}
