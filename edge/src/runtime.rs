use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum_server::Handle;
use axum_server::tls_rustls::RustlsConfig;
use thiserror::Error;
use tokio::task::{JoinError, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::admission::{AdmissionService, Clock, ReceiverBearer, SystemClock};
use crate::config::{ConfigError, EdgeConfig};
use crate::credentials::{CredentialError, CredentialStore};
use crate::delivery::DeliveryService;
use crate::spool::{Spool, SpoolError, SpoolKey};
use crate::tls::{TlsError, mtls_server_config_from_pem};

const MAX_SECRET_BYTES: usize = 1_048_576;
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

pub fn doctor(config: &EdgeConfig) -> Result<(), RuntimeError> {
    config.validate_runtime_files()?;
    let key_bytes = read_bounded(&config.spool_key_path)?;
    let _key: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| RuntimeError::InvalidSecret)?;
    let bearer_bytes = read_bounded(&config.receiver_bearer_path)?;
    let bearer_text =
        std::str::from_utf8(&bearer_bytes).map_err(|_| RuntimeError::InvalidSecret)?;
    ReceiverBearer::new(bearer_text).map_err(|_| RuntimeError::InvalidSecret)?;
    CredentialStore::open(&config.credential_store_path)?;
    mtls_server_config_from_pem(
        &read_bounded(&config.hub_server_certificate_path)?,
        &read_bounded(&config.hub_server_private_key_path)?,
        &read_bounded(&config.hub_client_ca_path)?,
    )?;
    Ok(())
}

pub async fn run_until_shutdown<F>(config: EdgeConfig, shutdown: F) -> Result<(), RuntimeError>
where
    F: Future<Output = ()> + Send,
{
    config.validate_runtime_files()?;
    let key_bytes = read_bounded(&config.spool_key_path)?;
    let key: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| RuntimeError::InvalidSecret)?;
    let bearer_bytes = read_bounded(&config.receiver_bearer_path)?;
    let bearer_text =
        std::str::from_utf8(&bearer_bytes).map_err(|_| RuntimeError::InvalidSecret)?;
    let bearer = ReceiverBearer::new(bearer_text).map_err(|_| RuntimeError::InvalidSecret)?;

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let now_ms = clock.now_ms();
    let spool = Spool::open(config.spool_config()?, SpoolKey::from_bytes(key), now_ms)?;
    let credentials = CredentialStore::open(&config.credential_store_path)?;
    let admission = AdmissionService::new(spool.clone(), bearer, clock.clone());
    let delivery = DeliveryService::new(spool.clone(), credentials, clock);

    let server_certificate = read_bounded(&config.hub_server_certificate_path)?;
    let server_private_key = read_bounded(&config.hub_server_private_key_path)?;
    let client_ca = read_bounded(&config.hub_client_ca_path)?;
    let tls_config =
        mtls_server_config_from_pem(&server_certificate, &server_private_key, &client_ca)?;

    let receiver_listener = tokio::net::TcpListener::bind(config.receiver_bind)
        .await
        .map_err(|_| RuntimeError::Bind)?;
    let cancellation = CancellationToken::new();
    let admission_cancellation = cancellation.clone();
    let hub_handle = Handle::new();
    let server_hub_handle = hub_handle.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        axum::serve(receiver_listener, admission.router())
            .with_graceful_shutdown(admission_cancellation.cancelled_owned())
            .await
            .map_err(|_| RuntimeError::Listener)
    });
    tasks.spawn(async move {
        axum_server::bind_rustls(config.hub_bind, RustlsConfig::from_config(tls_config))
            .handle(server_hub_handle)
            .serve(delivery.router().into_make_service())
            .await
            .map_err(|_| RuntimeError::Listener)
    });

    tokio::pin!(shutdown);
    let early_exit = tokio::select! {
        () = &mut shutdown => None,
        joined = tasks.join_next() => joined,
    };
    cancellation.cancel();
    hub_handle.graceful_shutdown(Some(SHUTDOWN_DEADLINE));

    let drain = async {
        while let Some(joined) = tasks.join_next().await {
            flatten_join(joined)?;
        }
        Ok::<(), RuntimeError>(())
    };
    let drained = tokio::time::timeout(SHUTDOWN_DEADLINE, drain).await;
    if drained.is_err() {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    spool.sync()?;

    if let Some(joined) = early_exit {
        flatten_join(joined)?;
        return Err(RuntimeError::ListenerStopped);
    }
    if let Ok(result) = drained {
        result?;
    }
    Ok(())
}

fn flatten_join(joined: Result<Result<(), RuntimeError>, JoinError>) -> Result<(), RuntimeError> {
    match joined {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Ok(()),
        Err(_) => Err(RuntimeError::Task),
    }
}

fn read_bounded(path: &std::path::Path) -> Result<Vec<u8>, RuntimeError> {
    let bytes = std::fs::read(path).map_err(|_| RuntimeError::Io)?;
    if bytes.is_empty() || bytes.len() > MAX_SECRET_BYTES {
        return Err(RuntimeError::InvalidSecret);
    }
    Ok(bytes)
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("configuration is invalid")]
    Config(#[from] ConfigError),
    #[error("spool is unavailable")]
    Spool(#[from] SpoolError),
    #[error("credential store is unavailable")]
    Credential(#[from] CredentialError),
    #[error("TLS configuration is invalid")]
    Tls(#[from] TlsError),
    #[error("runtime secret is invalid")]
    InvalidSecret,
    #[error("runtime input or output failed")]
    Io,
    #[error("listener bind failed")]
    Bind,
    #[error("listener failed")]
    Listener,
    #[error("listener stopped unexpectedly")]
    ListenerStopped,
    #[error("runtime task failed")]
    Task,
}
