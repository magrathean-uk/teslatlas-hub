// SPDX-License-Identifier: AGPL-3.0-only

// Test support of the C4 session tests (P2 onward): an in-process loopback
// WebSocket peer with a time-stamped log, a loopback token issuer, a test wall
// clock and jitter source, the audit-row readers and a rig that builds and runs a
// supervisor against them. Synthetic data only; nothing here touches the network.

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Condvar, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf, duplex},
    net::TcpListener,
    sync::{Mutex, Notify, mpsc, oneshot},
    task::{JoinHandle, JoinSet},
    time::{Instant, sleep, timeout},
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};
use url::Url;

use super::{
    AdmissionEvent, StreamEvent, StreamPowerGate, StreamRegion, StreamSupervisorError,
    TeslaStreamSupervisor, WriteRefusal,
    policy::{JitterSource, WallClock},
    session::{Connector, FrameCounts, Transport},
};
use crate::{
    credentials::LegacyAuthManager,
    db::HubStore,
    owner_api::{StreamVehicleId, VehicleId},
};

/// The session's tag, as in the spec's notation (section 9).
pub(super) const TAG: &str = "4242";
/// Spec notation T0: the sample time of the frames below, epoch milliseconds.
pub(super) const T0: i64 = 1_760_000_000_000;
/// Spec notation W: the wall clock the tests read unless stated.
pub(super) const W: i64 = T0 + 5_000;
/// Row V1 of the spec: elevation 25, est_lat 10.5, est_lng 20.25, est_range 230.5,
/// heading 182, odometer 12345.6, power 15, range 241.25, shift_state D, soc 80,
/// speed 30.
pub(super) const V1: &str = "25,10.5,20.25,230.5,182,12345.6,15,241.25,D,80,30";
/// Row PK of the spec: a parked frame.
pub(super) const PK: &str = "25,10.5,20.25,230.5,182,12345.6,0,241.25,P,80,0";

/// `U(t, row)` of the spec: one `data:update` frame for the session's tag.
pub(super) fn u(time: impl std::fmt::Display, row: &str) -> String {
    json!({"msg_type": "data:update", "tag": TAG, "value": format!("{time},{row}")}).to_string()
}

/// The `data:error` frame with this `error_type` and a `value` text that a
/// naive reader might act on (R28: the text is never the rule).
pub(super) fn error_frame(error_type: &str, text: &str) -> String {
    json!({"msg_type": "data:error", "tag": TAG, "error_type": error_type, "value": text})
        .to_string()
}

/// A `control:hello` with the given members; an absent one is omitted.
pub(super) fn hello_frame(code: Option<i64>, connection_timeout: Option<i64>) -> String {
    let mut members = serde_json::Map::new();
    members.insert("msg_type".to_owned(), json!("control:hello"));
    if let Some(code) = code {
        members.insert("code".to_owned(), json!(code));
    }
    if let Some(timeout_ms) = connection_timeout {
        members.insert("connection_timeout".to_owned(), json!(timeout_ms));
    }
    Value::Object(members).to_string()
}

/// A time-stamped log shared by the peer, the hook and the test. Time is the
/// async runtime's clock, so under paused time two entries are exactly as far
/// apart as the supervisor's waits.
#[derive(Clone, Default)]
pub(super) struct Log(Arc<LogInner>);

#[derive(Default)]
struct LogInner {
    entries: StdMutex<Vec<(Instant, String)>>,
    changed: Notify,
}

impl Log {
    pub(super) fn mark(&self, label: impl Into<String>) {
        self.0
            .entries
            .lock()
            .expect("log lock")
            .push((Instant::now(), label.into()));
        self.0.changed.notify_waiters();
    }

    pub(super) fn labels(&self) -> Vec<String> {
        self.0
            .entries
            .lock()
            .expect("log lock")
            .iter()
            .map(|(_, label)| label.clone())
            .collect()
    }

    /// Every entry, in order, with its time (P4: scenario tests pair the mark of a
    /// session's end with the next attempt's accept).
    pub(super) fn entries(&self) -> Vec<(Instant, String)> {
        self.0.entries.lock().expect("log lock").clone()
    }

    /// The times of every entry whose label starts with `prefix`.
    pub(super) fn times(&self, prefix: &str) -> Vec<Instant> {
        self.0
            .entries
            .lock()
            .expect("log lock")
            .iter()
            .filter(|(_, label)| label.starts_with(prefix))
            .map(|(at, _)| *at)
            .collect()
    }

    pub(super) fn count(&self, prefix: &str) -> usize {
        self.times(prefix).len()
    }

    /// The labels that start with `prefix`, with the prefix removed.
    pub(super) fn tails(&self, prefix: &str) -> Vec<String> {
        self.0
            .entries
            .lock()
            .expect("log lock")
            .iter()
            .filter_map(|(_, label)| label.strip_prefix(prefix).map(str::to_owned))
            .collect()
    }

    /// Waits until at least `n` entries start with `prefix`. The test fails if
    /// they have not appeared after 30 s of real time: a virtual-time timeout
    /// would be skipped over by the paused clock instead of failing the test.
    pub(super) async fn wait_for(&self, prefix: &str, n: usize) {
        let (expired, watchdog) = oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(30));
            let _ = expired.send(());
        });
        let wait = async {
            loop {
                let changed = self.0.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.count(prefix) >= n {
                    return;
                }
                changed.await;
            }
        };
        tokio::select! {
            () = wait => {}
            _ = watchdog => panic!("{n} log entries starting with {prefix:?} never appeared"),
        }
    }
}

/// A settable wall clock (1.2 `WallClock`).
pub(super) struct TestClock(AtomicI64);

impl TestClock {
    pub(super) fn new(ms: i64) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(ms)))
    }

    pub(super) fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl WallClock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A jitter source with scripted draws; every draw after the script is 0 ("jitter
/// draws are 0 unless stated", section 9).
pub(super) struct ScriptedJitter(VecDeque<f64>);

impl ScriptedJitter {
    pub(super) fn new(draws: &[f64]) -> Box<Self> {
        Box::new(Self(draws.iter().copied().collect()))
    }
}

impl JitterSource for ScriptedJitter {
    fn draw(&mut self) -> f64 {
        self.0.pop_front().unwrap_or(0.0)
    }
}

/// One connection the supervisor made to the peer: the server half of an
/// in-memory pipe.
pub(super) struct Conn {
    /// Zero-based order of the connection.
    pub(super) index: usize,
    io: DuplexStream,
    pub(super) log: Log,
}

impl Conn {
    /// Completes the WebSocket upgrade, logging the request target and whether
    /// it carried an `Authorization` header.
    #[allow(clippy::result_large_err)] // the handshake callback's error type is the library's
    pub(super) async fn ws(self) -> Ws {
        let log = self.log.clone();
        let inner = accept_hdr_async(self.io, |request: &Request, response: Response| {
            log.mark(format!("upgrade:{}", request.uri()));
            if let Some(host) = request.headers().get("host").and_then(|h| h.to_str().ok()) {
                log.mark(format!("host:{host}"));
            }
            if request.headers().contains_key("authorization") {
                log.mark("upgrade-authorization");
            }
            Ok(response)
        })
        .await
        .expect("the client's upgrade request");
        Ws {
            inner,
            log: self.log,
            reply_to_close: true,
        }
    }

    /// Reads the upgrade request and answers it with an HTTP status instead of
    /// 101, with an optional `Retry-After` header text.
    pub(super) async fn refuse_upgrade(mut self, status: u16, retry_after: Option<&str>) {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            match self.io.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(read) => request.extend_from_slice(&chunk[..read]),
            }
        }
        self.log.mark(format!("refused:{status}"));
        let mut response = format!("HTTP/1.1 {status} Synthetic\r\n");
        if let Some(value) = retry_after {
            response.push_str(&format!("Retry-After: {value}\r\n"));
        }
        response.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
        let _ = self.io.write_all(response.as_bytes()).await;
        let _ = self.io.shutdown().await;
    }

    /// Keeps the connection open without answering the upgrade.
    pub(super) async fn hold(self) {
        let _io = self.io;
        std::future::pending::<()>().await;
    }
}

/// The server side of one upgraded connection, with every message logged.
pub(super) struct Ws {
    pub(super) inner: WebSocketStream<DuplexStream>,
    pub(super) log: Log,
    /// Whether a Close from the client is answered (the default); a test of the
    /// bounded close turns it off.
    pub(super) reply_to_close: bool,
}

impl Ws {
    /// The next message, logged; `None` at a Close (logged with its code), the end
    /// of the stream or an error.
    pub(super) async fn message(&mut self) -> Option<Message> {
        match self.inner.next().await {
            Some(Ok(message)) => {
                match &message {
                    Message::Text(text) => self.log.mark(format!("frame:{}", text.as_str())),
                    Message::Binary(_) => self.log.mark("binary"),
                    Message::Ping(_) => self.log.mark("ping"),
                    Message::Pong(_) => self.log.mark("pong"),
                    Message::Close(frame) => {
                        let code = frame.as_ref().map_or_else(
                            || "none".to_owned(),
                            |frame| u16::from(frame.code).to_string(),
                        );
                        self.log.mark(format!("close:{code}"));
                        if self.reply_to_close {
                            // The library queues its Close reply; flushing sends it.
                            let _ = self.inner.flush().await;
                        }
                        return None;
                    }
                    Message::Frame(_) => {}
                }
                Some(message)
            }
            Some(Err(_)) | None => {
                self.log.mark("eof");
                None
            }
        }
    }

    /// The next text frame, skipping control frames.
    pub(super) async fn text(&mut self) -> Option<String> {
        loop {
            match self.message().await? {
                Message::Text(text) => return Some(text.as_str().to_owned()),
                _ => continue,
            }
        }
    }

    /// Reads the subscribe frame the client sends first, logs `subscribe-received`
    /// and `token:<token>` and returns it.
    pub(super) async fn subscribe(&mut self) -> Value {
        let frame = self.text().await.expect("the subscribe frame");
        let value: Value = serde_json::from_str(&frame).expect("a JSON subscribe frame");
        self.log.mark("subscribe-received");
        self.log.mark(format!(
            "token:{}",
            value["token"].as_str().unwrap_or("<missing>")
        ));
        value
    }

    pub(super) async fn send(&mut self, text: &str) {
        let _ = self.inner.send(Message::text(text.to_owned())).await;
    }

    pub(super) async fn send_binary(&mut self, bytes: &[u8]) {
        let _ = self.inner.send(Message::binary(bytes.to_vec())).await;
    }

    /// Sends a Close frame with this code and reason-less payload.
    pub(super) async fn close(&mut self, code: u16) {
        let _ = self
            .inner
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::from(code),
                reason: "".into(),
            })))
            .await;
    }

    /// Reads and logs everything until the client closes or the stream ends.
    pub(super) async fn drain(&mut self) {
        while self.message().await.is_some() {}
    }
}

/// The client half of a pipe, counting live connections: the count falls when the
/// supervisor drops its socket.
struct Tracked {
    inner: DuplexStream,
    active: Arc<AtomicUsize>,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AsyncRead for Tracked {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Tracked {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Buffer of each in-memory pipe, in bytes: far above the largest message of
/// the tests (64 KiB), so a write never waits for the peer to read.
const PIPE_BYTES: usize = 1 << 20;

/// An in-process WebSocket peer running a script per connection (IF e1 pattern,
/// over in-memory pipes). The supervisor reaches it through [`Peer::connector`]:
/// under paused time an in-memory pipe has none of the kernel latency that makes
/// the runtime skip ahead of a socket that is about to become ready.
pub(super) struct Peer {
    pub(super) log: Log,
    connector: Connector,
    max_active: Arc<AtomicUsize>,
    accept: Option<JoinHandle<()>>,
}

impl Peer {
    pub(super) async fn spawn<F, Fut>(script: F) -> Self
    where
        F: Fn(Conn) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self::spawn_with_pipe(PIPE_BYTES, script).await
    }

    /// A peer whose pipes buffer `pipe_bytes` each way: a peer that stops reading
    /// then blocks the client's write once that much is queued, as a full socket
    /// buffer does.
    pub(super) async fn spawn_with_pipe<F, Fut>(pipe_bytes: usize, script: F) -> Self
    where
        F: Fn(Conn) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let log = Log::default();
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let (made, mut accepted) = mpsc::unbounded_channel::<Conn>();
        let connector: Connector = {
            let log = log.clone();
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            let index = Arc::new(AtomicUsize::new(0));
            Arc::new(move || {
                let (client, server) = duplex(pipe_bytes);
                log.mark("accept");
                let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now_active, Ordering::SeqCst);
                let _ = made.send(Conn {
                    index: index.fetch_add(1, Ordering::SeqCst),
                    io: server,
                    log: log.clone(),
                });
                let tracked = Tracked {
                    inner: client,
                    active: Arc::clone(&active),
                };
                Box::pin(async move { Ok(Box::new(tracked) as Transport) })
            })
        };
        let accept = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            while let Some(conn) = accepted.recv().await {
                connections.spawn(script(conn));
            }
        });
        Self {
            log,
            connector,
            max_active,
            accept: Some(accept),
        }
    }

    /// A peer that refuses every connection, as a closed port does.
    pub(super) fn refusing() -> Self {
        let log = Log::default();
        let refused = log.clone();
        let connector: Connector = Arc::new(move || {
            refused.mark("connect-refused");
            Box::pin(async { Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)) })
        });
        Self {
            log,
            connector,
            max_active: Arc::new(AtomicUsize::new(0)),
            accept: None,
        }
    }

    /// The endpoint the supervisor is constructed with; the host and port are
    /// never connected to, because the connector replaces the transport.
    pub(super) fn url(&self) -> String {
        "ws://127.0.0.1:9/".to_owned()
    }

    pub(super) fn connector(&self) -> Connector {
        Arc::clone(&self.connector)
    }

    /// The most connections the supervisor held open at once.
    pub(super) fn max_concurrent(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        // Aborting the accept task drops its connection set, which aborts every
        // connection task.
        if let Some(accept) = &self.accept {
            accept.abort();
        }
    }
}

/// The longest, in real time, that a [`ClockHold`] keeps the paused clock back.
/// A safety net only: a hold that nobody releases ends the test's freeze and
/// lets it fail on its own assertions instead of hanging.
const CLOCK_HOLD_LIMIT: Duration = Duration::from_secs(10);

/// Keeps the paused runtime clock from advancing by itself while it lives.
///
/// Under `start_paused` the runtime moves the clock to the next timer whenever
/// it has nothing to run, including while it waits for a real loopback socket
/// (a token request to the [`Issuer`]) to become ready. Any timer armed at that
/// moment (the credential unit's 60 s bound on a token request, a sleep of the
/// test itself) can then fire although no virtual time was meant to pass, and
/// which one does depends on loopback latency. A blocking task holds the
/// automatic advance back for as long as it runs, so a hold is a blocking task
/// that waits to be released. It must be created inside the runtime.
pub(super) struct ClockHold {
    release: Arc<(StdMutex<bool>, Condvar)>,
}

impl ClockHold {
    pub(super) fn start() -> Self {
        let release = Arc::new((StdMutex::new(false), Condvar::new()));
        let waiting = Arc::clone(&release);
        drop(tokio::task::spawn_blocking(move || {
            let (flag, changed) = &*waiting;
            let released = flag.lock().expect("hold lock");
            let _ = changed.wait_timeout_while(released, CLOCK_HOLD_LIMIT, |released| !*released);
        }));
        Self { release }
    }
}

impl Drop for ClockHold {
    fn drop(&mut self) {
        let (flag, changed) = &*self.release;
        *flag.lock().expect("hold lock") = true;
        changed.notify_all();
    }
}

#[derive(Default)]
struct HoldState {
    /// A client of the issuer has announced an exchange and not yet ended it.
    armed: bool,
    hold: Option<ClockHold>,
}

/// The clock holds of the real HTTP exchanges between the credential unit and
/// the [`Issuer`], shared by the two sides. Virtual time must still pass while
/// the issuer waits out its `delay`, and the unit's own back-off waits are not
/// exchanges, so the clock is held only while bytes are in flight: from the
/// start of the client's refresh until the issuer has read the request, and
/// from the moment the issuer answers until the client's refresh has finished.
#[derive(Clone, Default)]
pub(super) struct ClockHolds(Arc<StdMutex<HoldState>>);

impl ClockHolds {
    /// The client starts a refresh: hold the clock until the issuer has the
    /// request.
    pub(super) fn exchange_begins(&self) {
        let mut state = self.0.lock().expect("hold state");
        state.armed = true;
        state.hold = Some(ClockHold::start());
    }

    /// The client's refresh is over: release whatever is held.
    pub(super) fn exchange_ends(&self) {
        let mut state = self.0.lock().expect("hold state");
        state.armed = false;
        state.hold = None;
    }

    /// The issuer has read the whole request: let virtual time pass while it
    /// waits out its delay.
    fn request_read(&self) {
        self.0.lock().expect("hold state").hold = None;
    }

    /// The issuer is about to write its answer: hold the clock until the client
    /// has read it, if a client announced the exchange.
    fn answering(&self) {
        let mut state = self.0.lock().expect("hold state");
        if state.armed {
            state.hold = Some(ClockHold::start());
        }
    }

    /// Runs one refresh the test makes itself under the same holds.
    pub(super) async fn held<T>(&self, work: impl Future<Output = T>) -> T {
        self.exchange_begins();
        let result = work.await;
        self.exchange_ends();
        result
    }
}

/// A loopback token issuer: answers every request to `/token` after `delay`
/// with a rotated pair, or with a status, and counts the requests.
pub(super) struct Issuer {
    pub(super) url: Url,
    pub(super) requests: Arc<AtomicUsize>,
    pub(super) log: Log,
    /// The clock holds of its exchanges (see [`ClockHolds`]); a client that
    /// announces its refresh through them is held back while bytes are in flight.
    pub(super) holds: ClockHolds,
    task: JoinHandle<()>,
}

impl Issuer {
    pub(super) async fn spawn(
        delay: Duration,
        access: &'static str,
        refresh: &'static str,
    ) -> Self {
        Self::spawn_with_status(delay, access, refresh, 200).await
    }

    pub(super) async fn spawn_with_status(
        delay: Duration,
        access: &'static str,
        refresh: &'static str,
        status: u16,
    ) -> Self {
        Self::spawn_with_error_body(delay, access, refresh, status, "{}").await
    }

    /// As [`Self::spawn_with_status`], with the body every non-200 answer carries
    /// (a token endpoint's error object, as a C5 R45 row names it).
    pub(super) async fn spawn_with_error_body(
        delay: Duration,
        access: &'static str,
        refresh: &'static str,
        status: u16,
        error_body: &'static str,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        let requests = Arc::new(AtomicUsize::new(0));
        let log = Log::default();
        let task_requests = Arc::clone(&requests);
        let task_log = log.clone();
        let holds = ClockHolds::default();
        let task_holds = holds.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            while let Ok((mut tcp, _)) = listener.accept().await {
                let requests = Arc::clone(&task_requests);
                let log = task_log.clone();
                let holds = task_holds.clone();
                connections.spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0_u8; 2048];
                    let body_start = loop {
                        match tcp.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                        }
                        if let Some(position) =
                            buffer.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            break position + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buffer[..body_start]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    while buffer.len() < body_start + length {
                        match tcp.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                        }
                    }
                    requests.fetch_add(1, Ordering::SeqCst);
                    holds.request_read();
                    log.mark("token-request");
                    sleep(delay).await;
                    holds.answering();
                    let body = if status == 200 {
                        json!({
                            "access_token": access,
                            "refresh_token": refresh,
                            "token_type": "Bearer",
                            "expires_in": 28_800,
                        })
                        .to_string()
                    } else {
                        error_body.to_owned()
                    };
                    let response = format!(
                        "HTTP/1.1 {status} Synthetic\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = tcp.write_all(response.as_bytes()).await;
                    let _ = tcp.shutdown().await;
                });
            }
        });
        Self {
            url: Url::parse(&format!("http://{addr}/")).expect("issuer URL"),
            requests,
            log,
            holds,
            task,
        }
    }
}

impl Drop for Issuer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The credential unit's environment on the runtime's clock (C5 R37): epoch seconds
/// are the real time at the start plus the (possibly paused) runtime time elapsed,
/// and a wait is a runtime sleep.
pub(super) struct RuntimeClock {
    epoch_at_start: i64,
    started: Instant,
}

impl RuntimeClock {
    pub(super) fn now() -> Self {
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after 1970")
            .as_secs();
        Self {
            epoch_at_start: i64::try_from(epoch).expect("epoch seconds"),
            started: Instant::now(),
        }
    }
}

impl crate::legacy_auth::attempt::AuthEnv for RuntimeClock {
    fn now(&self) -> i64 {
        self.epoch_at_start
            + i64::try_from(self.started.elapsed().as_secs()).expect("elapsed seconds")
    }

    fn sleep(&self, seconds: i64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let seconds = u64::try_from(seconds).unwrap_or(0);
        Box::pin(sleep(Duration::from_secs(seconds)))
    }

    fn spread_unit(&self) -> f64 {
        0.0
    }
}

/// The real clock in epoch seconds. The credential unit's test schedule is
/// compared with the system clock, which paused runtime time does not move, so
/// every schedule below is derived from it: a fixed epoch value would become
/// wrong on the day the real clock passes it (a "not due" pair turns due, then
/// expired).
pub(super) fn real_epoch_seconds() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs();
    i64::try_from(now).expect("epoch seconds")
}

/// The life of an ordinary access token, eight hours.
const TOKEN_LIFETIME_SECONDS: i64 = 28_800;
/// A token's refresh falls due six hours into its eight-hour life.
const REFRESH_AFTER_SECONDS: i64 = 21_600;
/// The lifetime of a pair whose refresh is due now but which has not expired:
/// a year, so a scenario on the runtime's own clock cannot run it out.
const DUE_PAIR_LIFETIME_SECONDS: i64 = 365 * 24 * 3_600;

/// `(expires_at, next_refresh_at)` of a pair whose refresh is not due: an
/// ordinary eight-hour token that has just been issued.
pub(super) fn schedule_not_due() -> (i64, i64) {
    let now = real_epoch_seconds();
    (now + TOKEN_LIFETIME_SECONDS, now + REFRESH_AFTER_SECONDS)
}

/// `(expires_at, next_refresh_at)` of a pair whose refresh is due now and whose
/// token has not expired.
pub(super) fn schedule_due() -> (i64, i64) {
    let now = real_epoch_seconds();
    (now + DUE_PAIR_LIFETIME_SECONDS, now - 1_000)
}

/// A credential unit over a temporary Hub store ([`Creds::over_a_store`]); the
/// store (also handed to the rig for its audit rows) lives as long as this does.
pub(super) struct StoredCreds {
    pub(super) creds: Creds,
    pub(super) store: HubStore,
    pub(super) _directory: tempfile::TempDir,
}

/// A manager built over a loopback issuer, with the counts the tests read.
pub(super) struct Creds {
    pub(super) manager: Arc<Mutex<LegacyAuthManager>>,
    pub(super) client: reqwest::Client,
    /// Every pair the manager persisted, in order.
    pub(super) persisted: Arc<StdMutex<Vec<(String, String)>>>,
    /// The clock holds of a real issuer's exchanges, when the test runs them
    /// under paused time ([`Creds::held_by`]).
    pub(super) holds: Option<ClockHolds>,
}

impl Creds {
    /// A manager holding this pair whose refresh is not due (the schedule lies
    /// ahead of the real clock by a normal token lifetime).
    pub(super) fn current(issuer: Url, access: &str) -> Self {
        Self::build(issuer, access, false)
    }

    /// A manager holding this pair whose refresh is due now.
    pub(super) fn due(issuer: Url, access: &str) -> Self {
        Self::build(issuer, access, true)
    }

    fn build(issuer: Url, access: &str, due: bool) -> Self {
        let persisted = Arc::new(StdMutex::new(Vec::new()));
        let manager = Self::manager_for(issuer, access, due, &persisted);
        Self {
            manager: Arc::new(Mutex::new(manager)),
            client: reqwest::Client::builder().build().expect("client"),
            persisted,
            holds: None,
        }
    }

    /// Announces this manager's refreshes to the issuer's clock holds: the
    /// supervisor's refresh (and a refresh the test wraps in
    /// [`ClockHolds::held`]) keeps the paused clock still while the real HTTP
    /// exchange is in flight.
    pub(super) fn held_by(mut self, issuer: &Issuer) -> Self {
        self.holds = Some(issuer.holds.clone());
        self
    }

    fn manager_for(
        issuer: Url,
        access: &str,
        due: bool,
        persisted: &Arc<StdMutex<Vec<(String, String)>>>,
    ) -> LegacyAuthManager {
        crate::crypto::install_default_provider();
        // A pair that is not due must satisfy the unit's own schedule rule (C5
        // R27): its stored `expires_at` lies at most a day and the allowance
        // ahead, or the unit treats the row as damaged and refreshes at once. So
        // it is written relative to the real clock, as a refresh would have.
        let schedule = if due {
            schedule_due()
        } else {
            schedule_not_due()
        };
        let auth = crate::legacy_auth::LegacyAuth::for_test(issuer, access, "test-refresh")
            .with_test_schedule(schedule.0, schedule.1);
        let sink = Arc::clone(persisted);
        LegacyAuthManager::for_test(
            auth,
            Arc::new(move |access, refresh| {
                sink.lock()
                    .expect("persist lock")
                    .push((access.to_owned(), refresh.to_owned()));
                Ok(())
            }),
        )
    }

    /// Replaces the manager's pair with one whose refresh is due now (P4). The
    /// test schedule reads the real clock, which paused time does not move, so a
    /// refresh that falls due part-way through a paused-time scenario is modelled
    /// by swapping the pair at that moment.
    pub(super) async fn make_due(&self, issuer: Url, access: &str) {
        let replacement = Self::manager_for(issuer, access, true, &self.persisted);
        *self.manager.lock().await = replacement;
    }

    /// A manager whose sensitive-access check fails while `authority` is false.
    pub(super) fn with_authority(issuer: Url, access: &str, authority: Arc<AtomicBool>) -> Self {
        crate::crypto::install_default_provider();
        let (expires_at, next_refresh_at) = schedule_not_due();
        let auth = crate::legacy_auth::LegacyAuth::for_test(issuer, access, "test-refresh")
            .with_test_schedule(expires_at, next_refresh_at);
        let manager = LegacyAuthManager::for_test_with_sensitive_access(
            auth,
            Arc::new(|_, _| Ok(())),
            Arc::new(move || {
                if authority.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(crate::credentials::CredentialError::SensitiveAccessUnavailable)
                }
            }),
        );
        Self {
            manager: Arc::new(Mutex::new(manager)),
            client: reqwest::Client::builder().build().expect("client"),
            persisted: Arc::new(StdMutex::new(Vec::new())),
            holds: None,
        }
    }

    /// The real credential unit over a temporary Hub store (C5 R57): the bounded
    /// synthetic pair `(access, "test-refresh")` is stored encrypted with a
    /// generation, on the schedule given, and the Provider-mode unit is built over
    /// it with the issuer's token endpoint and the runtime's clock. Row-8 receipts,
    /// input fences and the status row are the store's, as in production.
    pub(super) fn over_a_store(issuer: &Url, access: &str, schedule: (i64, i64)) -> StoredCreds {
        crate::crypto::install_default_provider();
        let directory = crate::private_tempdir().expect("temporary directory");
        let store = HubStore::initialize(directory.path()).expect("store");
        crate::teslamate_credentials::replace_key(directory.path(), b"stream-test-cloak-key")
            .expect("private key");
        let key = crate::teslamate_credentials::load_key(directory.path()).expect("key");
        let tokens = crate::credentials::OwnerTokens::from_secret_parts(
            access.to_owned(),
            "test-refresh".to_owned(),
        )
        .expect("a pair");
        let (cipher_access, cipher_refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens)
                .expect("encrypted pair");
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(
                    cipher_access,
                    cipher_refresh,
                    schedule.0,
                    schedule.1,
                )
                .expect("a schedule")
                .with_credential_generation(
                    crate::teslamate_token::legacy_refresh_credential_generation(&tokens),
                )
                .expect("a generation"),
            )
            .expect("the stored pair");
        let token_url = issuer.join("oauth2/v3/token").expect("token endpoint");
        let manager = LegacyAuthManager::from_hub_teslamate_store_for_test(
            store.clone(),
            directory.path(),
            &crate::legacy_auth::AuthEndpointConfig {
                mode: crate::legacy_auth::AuthMode::Provider,
                region: None,
                token_url: Some(token_url.to_string()),
                allow_loopback_http: true,
            },
            Some(token_url),
            false,
            Arc::new(RuntimeClock::now()),
        )
        .expect("the unit starts");
        StoredCreds {
            creds: Self::from_manager(manager),
            store,
            _directory: directory,
        }
    }

    /// A manager the test built: the real credential unit with whatever state the
    /// test put it in (C5).
    pub(super) fn from_manager(manager: LegacyAuthManager) -> Self {
        Self {
            manager: Arc::new(Mutex::new(manager)),
            client: reqwest::Client::builder().build().expect("client"),
            persisted: Arc::new(StdMutex::new(Vec::new())),
            holds: None,
        }
    }

    /// A manager with a refresh due now whose clock is the runtime's, so that
    /// paused time moves the unit's schedule, retries and minimum interval (C5
    /// R19, R31, R37) as it moves the supervisor's own waits.
    pub(super) fn due_on_the_runtime_clock(issuer: Url, access: &str) -> Self {
        crate::crypto::install_default_provider();
        let persisted = Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&persisted);
        let (expires_at, next_refresh_at) = schedule_due();
        let auth = crate::legacy_auth::LegacyAuth::for_test(issuer, access, "test-refresh")
            .with_test_schedule(expires_at, next_refresh_at);
        let manager = LegacyAuthManager::for_test_with_env(
            auth,
            Arc::new(move |access, refresh| {
                sink.lock()
                    .expect("persist lock")
                    .push((access.to_owned(), refresh.to_owned()));
                Ok(())
            }),
            Arc::new(RuntimeClock::now()),
        );
        // No network: a paused clock can cut real I/O short, so the issuer's 503 is
        // the unit's own test seam.
        manager.answer_token_requests_with(503);
        Self {
            manager: Arc::new(Mutex::new(manager)),
            client: reqwest::Client::builder().build().expect("client"),
            persisted,
            holds: None,
        }
    }

    /// A manager whose issuer is never contacted (the refresh is not due).
    pub(super) fn offline(access: &str) -> Self {
        Self::current(Url::parse("http://127.0.0.1:9/").expect("URL"), access)
    }
}

/// The supervisor and everything a test reads from a run.
pub(super) struct Rig {
    pub(super) endpoint: String,
    pub(super) creds: Creds,
    pub(super) store: Option<HubStore>,
    /// The run's write guard, when the test hands the audit one.
    pub(super) write_guard: Option<WriteRefusal>,
    /// The stream vehicle id, whose decimal text is the session's tag.
    pub(super) tag: u64,
    pub(super) clock: Arc<TestClock>,
    pub(super) jitter: Vec<f64>,
    /// The region the supervisor is built with; it is used only in logs (R1).
    pub(super) region: StreamRegion,
    pub(super) gate: Option<Arc<StreamPowerGate>>,
    pub(super) capacity: usize,
    pub(super) health: Option<Duration>,
    pub(super) terminal: Option<Arc<AtomicBool>>,
    /// How the supervisor reaches its peer; `None` connects to the endpoint over
    /// the network stack (real TCP).
    pub(super) connector: Option<Connector>,
    /// The log the admission hook writes `refresh` (and `hook:<name>`) into; set
    /// it to the peer's log to order hook and peer events.
    pub(super) log: Log,
    /// The frame counts of every ended session (R52), filled by the supervisor.
    pub(super) summaries: Arc<StdMutex<Vec<FrameCounts>>>,
}

impl Rig {
    pub(super) fn new(endpoint: String, creds: Creds) -> Self {
        Self {
            endpoint,
            creds,
            store: None,
            write_guard: None,
            tag: TAG.parse().expect("numeric tag"),
            clock: TestClock::new(W),
            jitter: Vec::new(),
            region: StreamRegion::Global,
            gate: None,
            capacity: 64,
            health: None,
            terminal: None,
            connector: None,
            log: Log::default(),
            summaries: Arc::default(),
        }
    }

    /// A rig whose supervisor talks to this in-memory peer, with the peer's log
    /// shared with the admission hook so hook and peer events are ordered.
    pub(super) fn for_peer(peer: &Peer, creds: Creds) -> Self {
        let mut rig = Self::new(peer.url(), creds);
        rig.connector = Some(peer.connector());
        rig.log = peer.log.clone();
        rig
    }

    pub(super) fn with_store(mut self) -> (Self, tempfile::TempDir) {
        let dir = crate::private_tempdir().expect("temporary directory");
        self.store = Some(HubStore::initialize(dir.path()).expect("store"));
        (self, dir)
    }

    /// Builds the supervisor with the test seams and starts its run as a task.
    pub(super) fn start(self) -> Running {
        let (events_tx, events) = mpsc::channel(self.capacity);
        let (shutdown, shutdown_rx) = oneshot::channel();
        let hooked = Arc::new(StdMutex::new(Vec::new()));
        let hook_events = Arc::clone(&hooked);
        let hook_log = self.log.clone();
        let hook_holds = self.creds.holds.clone();
        let mut supervisor = TeslaStreamSupervisor::new_legacy_auth_for_test(
            VehicleId::from_test(9),
            StreamVehicleId::from_test(self.tag),
            Arc::clone(&self.creds.manager),
            self.region,
            self.endpoint.clone(),
            self.creds.client.clone(),
            events_tx,
        )
        .expect("supervisor");
        if let Some(store) = self.store.clone() {
            supervisor = supervisor.with_audit_store(store);
        }
        if let Some(guard) = self.write_guard.clone() {
            supervisor = supervisor.with_write_guard(guard);
        }
        if let Some(gate) = self.gate.clone() {
            supervisor = supervisor.with_power_gate(gate);
        }
        if let Some(health) = self.health {
            supervisor = supervisor.with_health_timeout(health);
        }
        if let Some(flag) = self.terminal.clone() {
            supervisor = supervisor.with_terminal_credential(flag);
        }
        if let Some(connector) = self.connector.clone() {
            supervisor = supervisor.with_connector(connector);
        }
        let supervisor = supervisor
            .with_wall_clock(self.clock.clone())
            .with_summary_sink(Arc::clone(&self.summaries))
            .with_jitter(ScriptedJitter::new(&self.jitter))
            .with_admission_hook(Arc::new(move |event| {
                match event {
                    AdmissionEvent::RefreshStarted => {
                        if let Some(holds) = &hook_holds {
                            holds.exchange_begins();
                        }
                        hook_log.mark("refresh");
                    }
                    other => {
                        if let (AdmissionEvent::RefreshFinished, Some(holds)) = (event, &hook_holds)
                        {
                            holds.exchange_ends();
                        }
                        hook_log.mark(format!("hook:{other:?}"));
                    }
                }
                hook_events.lock().expect("hook lock").push(event);
            }));
        let task = tokio::spawn(supervisor.run(shutdown_rx));
        Running {
            events,
            shutdown: Some(shutdown),
            task: Some(task),
            hooked,
            clock: self.clock,
            creds: self.creds,
            summaries: self.summaries,
        }
    }
}

/// A started run.
pub(super) struct Running {
    pub(super) events: mpsc::Receiver<StreamEvent>,
    shutdown: Option<oneshot::Sender<()>>,
    pub(super) task: Option<JoinHandle<Result<(), StreamSupervisorError>>>,
    /// Every admission hook event, in order.
    pub(super) hooked: Arc<StdMutex<Vec<AdmissionEvent>>>,
    pub(super) clock: Arc<TestClock>,
    pub(super) creds: Creds,
    /// The frame counts of every ended session so far.
    pub(super) summaries: Arc<StdMutex<Vec<FrameCounts>>>,
}

impl Running {
    /// The next event within `within`; `None` if none came.
    pub(super) async fn event_within(&mut self, within: Duration) -> Option<StreamEvent> {
        timeout(within, self.events.recv()).await.ok().flatten()
    }

    /// The next event, which the test expects within two hours of paused time (the
    /// longest wait of any schedule is 3,600 s).
    pub(super) async fn event(&mut self) -> StreamEvent {
        self.event_within(Duration::from_secs(7_200))
            .await
            .expect("an event within two hours")
    }

    pub(super) fn signal_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    /// Sends the shutdown signal and awaits the task, which must end within 5 s of
    /// the signal (R11).
    pub(super) async fn shut_down(&mut self) -> Result<(), StreamSupervisorError> {
        self.signal_shutdown();
        let task = self.task.take().expect("task");
        timeout(Duration::from_secs(5), task)
            .await
            .expect("run returns within 5 s of the signal")
            .expect("run task did not panic")
    }

    /// Awaits the task's own end (an error, for tests of run termination).
    pub(super) async fn finished(&mut self, within: Duration) -> Result<(), StreamSupervisorError> {
        let task = self.task.take().expect("task");
        timeout(within, task)
            .await
            .expect("the run ended")
            .expect("run task did not panic")
    }

    /// The frame counts of every session that has ended, in order (R52).
    pub(super) fn session_counts(&self) -> Vec<FrameCounts> {
        self.summaries.lock().expect("summaries lock").clone()
    }

    pub(super) fn hook_events(&self) -> Vec<AdmissionEvent> {
        self.hooked.lock().expect("hook lock").clone()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// One outbound-request row, as the audit table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RequestRow {
    pub(super) operation: String,
    pub(super) outcome: String,
    pub(super) http_status: Option<i64>,
    pub(super) retry_after_seconds: Option<i64>,
    pub(super) transport: String,
    pub(super) safety_class: String,
    pub(super) precondition: String,
    pub(super) correlation_id: String,
}

pub(super) fn request_rows(store: &HubStore) -> Vec<RequestRow> {
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(
            "SELECT operation, outcome, http_status, retry_after_seconds, transport,
                    safety_class, precondition, correlation_id
               FROM outbound_request_receipts ORDER BY id",
        )
        .expect("statement");
    statement
        .query_map([], |row| {
            Ok(RequestRow {
                operation: row.get(0)?,
                outcome: row.get(1)?,
                http_status: row.get(2)?,
                retry_after_seconds: row.get(3)?,
                transport: row.get(4)?,
                safety_class: row.get(5)?,
                precondition: row.get(6)?,
                correlation_id: row.get(7)?,
            })
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows")
}

/// Operation and outcome of every request row, in id order.
pub(super) fn request_outcomes(store: &HubStore) -> Vec<(String, String)> {
    request_rows(store)
        .into_iter()
        .map(|row| (row.operation, row.outcome))
        .collect()
}

/// One session row: its outcome and whether it links an unsubscribe receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionRow {
    pub(super) outcome: String,
    pub(super) unsubscribe_receipt_id: Option<i64>,
    pub(super) correlation_id: String,
}

pub(super) fn session_rows(store: &HubStore) -> Vec<SessionRow> {
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(
            "SELECT outcome, unsubscribe_receipt_id, correlation_id
               FROM stream_session_receipts ORDER BY id",
        )
        .expect("statement");
    statement
        .query_map([], |row| {
            Ok(SessionRow {
                outcome: row.get(0)?,
                unsubscribe_receipt_id: row.get(1)?,
                correlation_id: row.get(2)?,
            })
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows")
}

/// Id of the request row of this operation (the first, in id order).
pub(super) fn request_id(store: &HubStore, operation: &str) -> Option<i64> {
    let connection = store.open().expect("connection");
    connection
        .query_row(
            "SELECT id FROM outbound_request_receipts WHERE operation = ?1 ORDER BY id LIMIT 1",
            [operation],
            |row| row.get(0),
        )
        .ok()
}
