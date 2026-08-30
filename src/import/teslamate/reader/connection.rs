// SPDX-License-Identifier: AGPL-3.0-only

pub(crate) struct TeslaMateSnapshotSession {
    client: Option<Client>,
    connection_task: Option<tokio::task::JoinHandle<()>>,
}

/// The encrypted TeslaMate legacy OAuth pair. Ciphertext is sensitive even
/// though it is not plaintext, so this type deliberately has no derived
/// formatter or serializer.
pub struct TeslaMateLegacyTokenCiphertexts {
    pub access: Vec<u8>,
    pub refresh: Vec<u8>,
}

impl Zeroize for TeslaMateLegacyTokenCiphertexts {
    fn zeroize(&mut self) {
        self.access.zeroize();
        self.refresh.zeroize();
    }
}

impl Drop for TeslaMateLegacyTokenCiphertexts {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl TeslaMateLegacyTokenCiphertexts {
    pub fn into_parts(mut self) -> (Vec<u8>, Vec<u8>) {
        (
            std::mem::take(&mut self.access),
            std::mem::take(&mut self.refresh),
        )
    }
}

const MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES_I64: i64 = MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES as i64;

const PRIVATE_LEGACY_TOKENS_SQL: &str = "SELECT \"token\".\"access\" AS \"access\", \"token\".\"refresh\" AS \"refresh\" \
     FROM \"private\".\"tokens\" AS \"token\" ORDER BY \"token\".\"id\" ASC LIMIT 2";
const PRIVATE_LEGACY_TOKENS_EXISTS_SQL: &str =
    "SELECT pg_catalog.to_regclass('private.tokens') IS NOT NULL AS \"private_tokens_exists\"";
const PRIVATE_LEGACY_TOKEN_LENGTHS_SQL: &str = "SELECT pg_catalog.octet_length(\"token\".\"access\")::bigint AS \"access_length\", pg_catalog.octet_length(\"token\".\"refresh\")::bigint AS \"refresh_length\" FROM \"private\".\"tokens\" AS \"token\" ORDER BY \"token\".\"id\" ASC LIMIT 2";

impl std::fmt::Debug for TeslaMateLegacyTokenCiphertexts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TeslaMateLegacyTokenCiphertexts([redacted])")
    }
}

impl TeslaMateSnapshotSession {
    fn new(client: Client, connection_task: tokio::task::JoinHandle<()>) -> Self {
        Self {
            client: Some(client),
            connection_task: Some(connection_task),
        }
    }

    #[cfg(test)]
    fn for_connection_task(connection_task: tokio::task::JoinHandle<()>) -> Self {
        Self {
            client: None,
            connection_task: Some(connection_task),
        }
    }

    pub(crate) fn client(&self) -> &Client {
        self.client
            .as_ref()
            .expect("snapshot client exists until session finish")
    }

    pub(crate) async fn finish(mut self) -> Result<(), TeslaMateReaderError> {
        let rollback = match self.client.as_ref() {
            Some(client) => {
                match timeout(SNAPSHOT_ROLLBACK_TIMEOUT, client.batch_execute("ROLLBACK")).await {
                    Ok(result) => result.map_err(TeslaMateReaderError::Postgres),
                    Err(_) => Err(TeslaMateReaderError::SnapshotRollbackTimedOut),
                }
            }
            None => Ok(()),
        };
        self.client.take();
        let shutdown = finish_connection_task(
            &mut self.connection_task,
            SNAPSHOT_CONNECTION_SHUTDOWN_TIMEOUT,
        )
        .await;
        rollback.and(shutdown)
    }
}

impl Drop for TeslaMateSnapshotSession {
    fn drop(&mut self) {
        self.client.take();
        abort_connection_task(&mut self.connection_task);
    }
}

fn abort_connection_task(task: &mut Option<tokio::task::JoinHandle<()>>) {
    if let Some(task) = task.take() {
        task.abort();
    }
}

async fn finish_connection_task(
    task: &mut Option<tokio::task::JoinHandle<()>>,
    shutdown_timeout: Duration,
) -> Result<(), TeslaMateReaderError> {
    let Some(connection_task) = task.as_mut() else {
        return Ok(());
    };
    match timeout(shutdown_timeout, connection_task).await {
        Ok(result) => {
            task.take();
            result.map_err(|error| TeslaMateReaderError::SnapshotConnectionTaskFailed {
                cancelled: error.is_cancelled(),
                panicked: error.is_panic(),
            })
        }
        Err(_) => {
            task.as_ref()
                .expect("connection task remains owned after shutdown timeout")
                .abort();
            let aborted = timeout(
                shutdown_timeout,
                task.as_mut()
                    .expect("connection task remains owned while aborting"),
            )
            .await;
            match aborted {
                Ok(Ok(())) => {
                    task.take();
                    Err(TeslaMateReaderError::SnapshotConnectionShutdownTimedOut)
                }
                Ok(Err(error)) if error.is_cancelled() => {
                    task.take();
                    Err(TeslaMateReaderError::SnapshotConnectionShutdownTimedOut)
                }
                Ok(Err(error)) => {
                    task.take();
                    Err(TeslaMateReaderError::SnapshotConnectionTaskFailed {
                        cancelled: error.is_cancelled(),
                        panicked: error.is_panic(),
                    })
                }
                Err(_) => {
                    let connection_task = task
                        .take()
                        .expect("timed-out connection task remains owned for draining");
                    spawn_connection_task_drain(connection_task);
                    Err(TeslaMateReaderError::SnapshotConnectionAbortTimedOut)
                }
            }
        }
    }
}

fn spawn_connection_task_drain(connection_task: tokio::task::JoinHandle<()>) {
    // A JoinHandle detaches its task when dropped. Keep the non-cooperative
    // aborted task owned by a runtime task until it actually reaches a terminal
    // state; the bounded caller can then report the timeout without leaking an
    // unowned PostgreSQL connection task.
    tokio::spawn(async move {
        let _ = connection_task.await;
    });
}

/// A read-only source transaction that keeps one PostgreSQL snapshot available
/// for bounded capture lanes. Dropping the lease releases the source snapshot;
/// callers must retain it until every lane has completed.
pub(crate) struct ExportedSnapshotLease {
    session: TeslaMateSnapshotSession,
    snapshot_id: String,
}

impl ExportedSnapshotLease {
    pub(crate) fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }

    /// Run one bounded read in the owner transaction for this exact exported
    /// snapshot. Capture lanes must finish before the lease does.
    pub(crate) fn client(&self) -> &Client {
        self.session.client()
    }

    pub(crate) async fn finish(self) -> Result<(), TeslaMateReaderError> {
        self.session.finish().await
    }
}

async fn connect_source(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    limits: TeslaMateReadLimits,
) -> Result<(Client, tokio::task::JoinHandle<()>), TeslaMateReaderError> {
    if !postgres_source_queries_allowed() {
        return Err(TeslaMateReaderError::SourceQueriesForbiddenAfterHandoff);
    }
    let user = source
        .user()
        .ok_or(TeslaMateReaderError::SourceUserRequired)?;
    let mut configuration = Config::new();
    configuration
        .host(source.connection_host())
        .port(source.port())
        .user(user)
        .password(password.as_str())
        .dbname(source.database_name());
    if matches!(source_transport(source), SourceTransport::PlaintextLoopback) {
        configuration.ssl_mode(SslMode::Disable);
        let (client, connection) = timeout(limits.connect_timeout, configuration.connect(NoTls))
            .await
            .map_err(|_| TeslaMateReaderError::ConnectTimedOut)??;
        let connection_task = tokio::spawn(async move {
            let _ = connection.await;
        });
        return Ok((client, connection_task));
    }

    crate::crypto::install_default_provider();
    let (tls, certificate_errors) = MakeRustlsConnect::with_native_certs()
        .map_err(|_| TeslaMateReaderError::NativeTrustStoreUnavailable)?;
    if !certificate_errors.is_empty() {
        tracing::warn!(
            count = certificate_errors.len(),
            "some native TLS certificates could not be loaded"
        );
    }
    configuration.ssl_mode(SslMode::Require);

    let (client, connection) = timeout(limits.connect_timeout, configuration.connect(tls))
        .await
        .map_err(|_| TeslaMateReaderError::ConnectTimedOut)??;
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, connection_task))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceTransport {
    PlaintextLoopback,
    Rustls,
}

fn source_transport(source: &ReadOnlySource) -> SourceTransport {
    if source.is_loopback() {
        SourceTransport::PlaintextLoopback
    } else {
        SourceTransport::Rustls
    }
}
