// SPDX-License-Identifier: AGPL-3.0-only

pub(crate) struct SourceDbSnapshotSession {
    client: Option<Client>,
    connection_task: Option<tokio::task::JoinHandle<()>>,
}

impl SourceDbSnapshotSession {
    pub(crate) fn new(client: Client, connection_task: tokio::task::JoinHandle<()>) -> Self {
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

    pub(crate) async fn finish(mut self) -> Result<(), SourceDbReaderError> {
        let rollback = match self.client.as_ref() {
            Some(client) => {
                match timeout(SNAPSHOT_ROLLBACK_TIMEOUT, client.batch_execute("ROLLBACK")).await {
                    Ok(result) => result.map_err(SourceDbReaderError::Postgres),
                    Err(_) => Err(SourceDbReaderError::SnapshotRollbackTimedOut),
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

impl Drop for SourceDbSnapshotSession {
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
) -> Result<(), SourceDbReaderError> {
    let Some(connection_task) = task.as_mut() else {
        return Ok(());
    };
    match timeout(shutdown_timeout, connection_task).await {
        Ok(result) => {
            task.take();
            result.map_err(|error| SourceDbReaderError::SnapshotConnectionTaskFailed {
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
                    Err(SourceDbReaderError::SnapshotConnectionShutdownTimedOut)
                }
                Ok(Err(error)) if error.is_cancelled() => {
                    task.take();
                    Err(SourceDbReaderError::SnapshotConnectionShutdownTimedOut)
                }
                Ok(Err(error)) => {
                    task.take();
                    Err(SourceDbReaderError::SnapshotConnectionTaskFailed {
                        cancelled: error.is_cancelled(),
                        panicked: error.is_panic(),
                    })
                }
                Err(_) => {
                    let connection_task = task
                        .take()
                        .expect("timed-out connection task remains owned for draining");
                    spawn_connection_task_drain(connection_task);
                    Err(SourceDbReaderError::SnapshotConnectionAbortTimedOut)
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
    session: SourceDbSnapshotSession,
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

    pub(crate) async fn finish(self) -> Result<(), SourceDbReaderError> {
        self.session.finish().await
    }
}

pub(crate) async fn connect_source(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    limits: SourceDbReadLimits,
) -> Result<(Client, tokio::task::JoinHandle<()>), SourceDbReaderError> {
    if !postgres_source_queries_allowed() {
        return Err(SourceDbReaderError::SourceQueriesForbiddenAfterHandoff);
    }
    let user = source
        .user()
        .ok_or(SourceDbReaderError::SourceUserRequired)?;
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
            .map_err(|_| SourceDbReaderError::ConnectTimedOut)??;
        let connection_task = tokio::spawn(drive_source_connection(connection));
        return Ok((client, connection_task));
    }

    crate::crypto::install_default_provider();
    let (tls, certificate_errors) = MakeRustlsConnect::with_native_certs()
        .map_err(|_| SourceDbReaderError::NativeTrustStoreUnavailable)?;
    if !certificate_errors.is_empty() {
        tracing::warn!(
            count = certificate_errors.len(),
            "some native TLS certificates could not be loaded"
        );
    }
    configuration.ssl_mode(SslMode::Require);

    let (client, connection) = timeout(limits.connect_timeout, configuration.connect(tls))
        .await
        .map_err(|_| SourceDbReaderError::ConnectTimedOut)??;
    let connection_task = tokio::spawn(drive_source_connection(connection));
    Ok((client, connection_task))
}

/// Drive a source connection until it ends. The driver's own `Future` writes
/// the text of every server notice to the log; the source database chooses
/// that text, so notices are read and dropped here instead.
async fn drive_source_connection<S, T>(mut connection: tokio_postgres::Connection<S, T>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(Ok(_)) =
        std::future::poll_fn(|context| connection.poll_message(context)).await
    {}
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

#[cfg(test)]
mod source_wire_tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    use tokio_postgres::tls::NoTlsStream;

    use super::*;

    pub(super) const SENTINEL: &str = "harmless-fix-b5-sentinel";

    fn backend(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut message = vec![tag];
        let length = i32::try_from(body.len() + 4).expect("short message");
        message.extend_from_slice(&length.to_be_bytes());
        message.extend_from_slice(body);
        message
    }

    /// An ErrorResponse or NoticeResponse body whose message, detail and hint
    /// all carry `text`.
    fn server_fields(severity: &str, code: &str, text: &str) -> Vec<u8> {
        let mut body = Vec::new();
        for (field, value) in [
            (b'S', severity),
            (b'V', severity),
            (b'C', code),
            (b'M', text),
            (b'D', text),
            (b'H', text),
        ] {
            body.push(field);
            body.extend_from_slice(value.as_bytes());
            body.push(0);
        }
        body.push(0);
        body
    }

    pub(super) fn notice(text: &str) -> Vec<u8> {
        backend(b'N', &server_fields("NOTICE", "01000", text))
    }

    pub(super) fn refusal(code: &str, text: &str) -> Vec<u8> {
        let mut reply = backend(b'E', &server_fields("ERROR", code, text));
        reply.extend(backend(b'Z', b"I"));
        reply
    }

    /// Read the startup message and accept the client without a password.
    pub(super) async fn accept(server: &mut DuplexStream) {
        let length = server.read_i32().await.expect("startup length");
        let mut startup = vec![0; usize::try_from(length - 4).expect("startup body")];
        server.read_exact(&mut startup).await.expect("startup body");
        let mut ready = backend(b'R', &0_i32.to_be_bytes());
        ready.extend(backend(b'Z', b"I"));
        server.write_all(&ready).await.expect("authentication");
    }

    /// Read one typed frontend message, such as a simple query.
    pub(super) async fn read_request(server: &mut DuplexStream) {
        server.read_u8().await.expect("message type");
        let length = server.read_i32().await.expect("message length");
        let mut body = vec![0; usize::try_from(length - 4).expect("message body")];
        server.read_exact(&mut body).await.expect("message body");
    }

    /// An in-memory PostgreSQL transport; the test plays the server on the
    /// returned end.
    pub(super) async fn connect_in_memory<F>(
        server: impl FnOnce(DuplexStream) -> F,
    ) -> (Client, tokio_postgres::Connection<DuplexStream, NoTlsStream>)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let (client_end, server_end) = tokio::io::duplex(64 * 1024);
        tokio::spawn(server(server_end));
        let mut configuration = Config::new();
        configuration
            .user("reader")
            .dbname("sourcedb")
            .ssl_mode(SslMode::Disable);
        configuration
            .connect_raw(client_end, NoTls)
            .await
            .expect("in-memory startup")
    }

    fn public_chain(error: &dyn std::error::Error) -> String {
        let mut lines = vec![error.to_string()];
        let mut cause = error.source();
        while let Some(next) = cause {
            lines.push(next.to_string());
            cause = next.source();
        }
        lines.join("\n")
    }

    /// The operator sees the SQLSTATE of a server refusal, never its text, in
    /// every error and cause that `report_outcome` prints; a server notice is
    /// consumed by the driver instead of being written to the log.
    #[tokio::test]
    async fn source_server_text_stays_out_of_public_errors() {
        let (client, connection) = connect_in_memory(|mut server| async move {
            accept(&mut server).await;
            server.write_all(&notice(SENTINEL)).await.expect("notice");
            for _ in 0..2 {
                read_request(&mut server).await;
                server
                    .write_all(&refusal("42P01", SENTINEL))
                    .await
                    .expect("refusal");
            }
            let _ = server.read_u8().await;
        })
        .await;
        let driver = tokio::spawn(drive_source_connection(connection));
        let reader = SourceDbReaderError::from(
            client
                .batch_execute("SELECT 1")
                .await
                .expect_err("the server refuses"),
        );
        let direct = crate::source_db_direct::SourceDbDirectError::from(
            client
                .batch_execute("SELECT 1")
                .await
                .expect_err("the server refuses"),
        );
        for public in [public_chain(&reader), public_chain(&direct)] {
            assert!(public.contains("SQLSTATE 42P01"), "{public}");
            assert!(!public.contains(SENTINEL), "{public}");
        }
        drop(client);
        driver.await.expect("the driver ends with the connection");
    }
}
