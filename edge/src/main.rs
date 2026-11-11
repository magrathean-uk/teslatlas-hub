#![forbid(unsafe_code)]

use std::error::Error;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use teslatlas_edge::config::{EdgeConfig, initialize, rotate_receiver_token};
use teslatlas_edge::credentials::CredentialStore;
use teslatlas_edge::runtime::{doctor, run_until_shutdown};
use teslatlas_edge::spool::SPOOL_FORMAT_VERSION;

const DEFAULT_CREDENTIAL_TTL_SECONDS: u64 = 90 * 24 * 60 * 60;

#[derive(Debug, Parser)]
#[command(version, about = "Teslatlas user-operated Fleet Telemetry ingress")]
struct Cli {
    #[arg(
        long,
        value_name = "PATH",
        default_value = "/etc/teslatlas-edge/config.toml"
    )]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the maximum supported on-disk spool format for launch guards.
    StorageFormat,
    /// Create the Edge spool key, receiver bearer, and empty Hub credential store.
    Init,
    /// Validate configured files without opening listeners.
    Doctor,
    /// Run the loopback receiver and public mTLS Hub listener.
    Serve,
    /// Manage scoped Hub bearer credentials.
    Credential {
        #[command(subcommand)]
        command: CredentialCommand,
    },
    /// Manage the loopback receiver bearer.
    ReceiverToken {
        #[command(subcommand)]
        command: ReceiverTokenCommand,
    },
}

#[derive(Debug, Subcommand)]
enum CredentialCommand {
    Enrol {
        label: String,
        #[arg(long, default_value_t = DEFAULT_CREDENTIAL_TTL_SECONDS)]
        ttl_seconds: u64,
    },
    Rotate {
        credential_id: String,
        #[arg(long, default_value_t = 300)]
        overlap_seconds: u64,
        #[arg(long, default_value_t = DEFAULT_CREDENTIAL_TTL_SECONDS)]
        ttl_seconds: u64,
    },
    Revoke {
        credential_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum ReceiverTokenCommand {
    Rotate,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    if matches!(&cli.command, Command::StorageFormat) {
        println!("{SPOOL_FORMAT_VERSION}");
        return Ok(());
    }
    let config = EdgeConfig::load(&cli.config)?;
    match cli.command {
        Command::StorageFormat => unreachable!(),
        Command::Init => {
            initialize(&config)?;
            println!("initialized");
        }
        Command::Doctor => {
            doctor(&config)?;
            println!("ok");
        }
        Command::Serve => run_until_shutdown(config, shutdown_signal()).await?,
        Command::Credential { command } => {
            config.validate_runtime_files()?;
            manage_credential(&config, command)?;
        }
        Command::ReceiverToken {
            command: ReceiverTokenCommand::Rotate,
        } => {
            let issued = rotate_receiver_token(&config)?;
            println!("{}", serde_json::json!({"receiver_token": issued.token()}));
        }
    }
    Ok(())
}

fn manage_credential(
    config: &EdgeConfig,
    command: CredentialCommand,
) -> Result<(), Box<dyn Error>> {
    let store = CredentialStore::open(&config.credential_store_path)?;
    let now_ms = now_ms();
    match command {
        CredentialCommand::Enrol { label, ttl_seconds } => {
            let issued = store.enrol(&label, now_ms, seconds_to_ms(ttl_seconds)?)?;
            println!(
                "{}",
                serde_json::json!({
                    "credential_id": issued.credential_id(),
                    "token": issued.token()
                })
            );
        }
        CredentialCommand::Rotate {
            credential_id,
            overlap_seconds,
            ttl_seconds,
        } => {
            let issued = store.rotate(
                &credential_id,
                now_ms,
                seconds_to_ms(overlap_seconds)?,
                seconds_to_ms(ttl_seconds)?,
            )?;
            println!(
                "{}",
                serde_json::json!({
                    "credential_id": issued.credential_id(),
                    "token": issued.token()
                })
            );
        }
        CredentialCommand::Revoke { credential_id } => {
            store.revoke(&credential_id, now_ms)?;
            println!("revoked");
        }
    }
    Ok(())
}

fn seconds_to_ms(seconds: u64) -> Result<i64, Box<dyn Error>> {
    seconds
        .checked_mul(1_000)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| "duration is too large".into())
}

fn now_ms() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let terminate = async {
            if let Ok(mut signal) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                signal.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            () = terminate => {},
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
