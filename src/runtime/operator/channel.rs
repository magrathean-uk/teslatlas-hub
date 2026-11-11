// SPDX-License-Identifier: AGPL-3.0-only

//! The local control channel between operator commands and the running
//! service (ruling R5).
//!
//! The service binds one Unix socket inside a private directory of its data
//! directory and answers one bounded JSON request per connection, which must
//! arrive whole within an absolute deadline. Each end authenticates the
//! other as this user's Hub executable: the command before it reads its
//! input, the service before it reads or acts on a request. A command
//! connects before it reads a secret path or standard input: no socket, or no
//! listener behind it, is `service_not_running`, so a refused command has read
//! nothing and opened no network connection. Requests carry bounded material
//! only (a grant, a trust document, the sign-in code material, recovery file
//! paths, the purge confirmation); no request carries an access or refresh
//! token.

use std::{
    fmt,
    io::{ErrorKind, Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use super::admission::{OperatorRefusal, SERVICE_NOT_RUNNING};

/// Largest request the service reads (a 4 KiB grant fits encoded).
pub const REQUEST_MAX_BYTES: usize = 16 * 1024;
/// Largest response a command reads.
pub const RESPONSE_MAX_BYTES: usize = 256 * 1024;
/// How long the service waits for a whole request to arrive, from accept:
/// an absolute deadline, so a client that trickles bytes cannot hold a
/// worker longer (review finding 6).
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
/// Requests the service handles at once; one more is refused busy, so a slow
/// client never stands between the operator and `window close`.
pub const MAX_ACTIVE_REQUESTS: usize = 4;
/// How long a stopping service waits for active requests it has interrupted.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
/// Accept poll interval of the service thread.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// Longest socket path macOS accepts, less its terminator.
const SOCKET_PATH_MAX: usize = 103;

pub const CHANNEL_UNSAFE: &str = "control_channel_unsafe";
pub const CHANNEL_BUSY: &str = "control_channel_busy";
pub const CHANNEL_FAILED: &str = "control_channel_failed";
/// The listener on the socket is not the admitted resident service.
pub const SERVICE_UNAUTHENTICATED: &str = "service_unauthenticated";
/// The process that sent a request is not the Hub's operator command: the
/// service read nothing of it and did nothing.
pub const PEER_UNAUTHENTICATED: &str = "peer_unauthenticated";

/// The private directory that holds the socket.
pub fn channel_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("operator")
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    channel_dir(data_dir).join("control.sock")
}

/// The sign-in code material the owner's browser session produced. It is
/// cleared on drop and never printed.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupMaterial {
    pub code: String,
    pub code_verifier: String,
    pub redirect_uri: String,
}

impl fmt::Debug for SetupMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SetupMaterial { .. }")
    }
}

impl Drop for SetupMaterial {
    fn drop(&mut self) {
        self.code.zeroize();
        self.code_verifier.zeroize();
        self.redirect_uri.zeroize();
    }
}

/// One request. Grants and trust documents travel base64-encoded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperatorRequest {
    WindowStatus,
    WindowTrust {
        document: String,
    },
    WindowOpen {
        grant: String,
    },
    WindowClose,
    Setup {
        material: SetupMaterial,
        vehicle_id: Option<i64>,
        all_vehicles: bool,
    },
    RecoveryExport {
        destination: PathBuf,
        recovery_key_file: PathBuf,
    },
    RecoveryRestore {
        source: PathBuf,
        recovery_key_file: PathBuf,
    },
    /// Lists the refused credentials, or with `confirm` removes them (M02-T11).
    PurgeRefusedCredentials {
        confirm: bool,
    },
    /// Runs one encrypted backup now (M06-T03). The paths default to the
    /// configuration's; the password stays in its file, which the service reads.
    BackupNow {
        #[serde(default)]
        destination: Option<PathBuf>,
        #[serde(default)]
        password_file: Option<PathBuf>,
    },
    /// Reads the backup schedule's state (M06-T04): read-only, for the Mac
    /// app's Backups section. It names no path and no password.
    BackupStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OperatorResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

impl OperatorResponse {
    fn from_result(result: Result<serde_json::Value, OperatorRefusal>) -> Self {
        match result {
            Ok(value) => Self {
                ok: true,
                code: None,
                message: None,
                result: Some(value),
            },
            Err(refusal) => Self {
                ok: false,
                code: Some(refusal.code),
                message: Some(refusal.message),
                result: None,
            },
        }
    }
}

/// The service side of the channel.
pub trait OperatorHandler: Send + Sync {
    fn handle(&self, request: OperatorRequest) -> Result<serde_json::Value, OperatorRefusal>;
}

fn unsafe_channel(what: &str) -> OperatorRefusal {
    OperatorRefusal::new(CHANNEL_UNSAFE, format!("the control channel {what}"))
}

fn failed(what: &str) -> OperatorRefusal {
    OperatorRefusal::new(CHANNEL_FAILED, format!("the control channel {what}"))
}

/// The channel directory must be a real directory owned by this user and
/// closed to everyone else.
fn check_private_dir(dir: &Path) -> Result<bool, OperatorRefusal> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir()
                || metadata.uid() != rustix::process::getuid().as_raw()
                || metadata.mode() & 0o077 != 0
            {
                return Err(unsafe_channel("directory is not private to this user"));
            }
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(_) => Err(unsafe_channel("directory cannot be inspected")),
    }
}

fn unauthenticated(what: &str) -> OperatorRefusal {
    OperatorRefusal::new(
        SERVICE_UNAUTHENTICATED,
        format!("the process on the control channel {what}; nothing was sent"),
    )
}

/// Refusal the service answers a requester it does not admit; it has read
/// nothing of the request and acted on nothing.
fn peer_refused(what: &str) -> OperatorRefusal {
    OperatorRefusal::new(
        PEER_UNAUTHENTICATED,
        format!("the requesting process {what}; nothing was done"),
    )
}

/// Why the process at the other end of a connection is not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerFault {
    /// The channel itself could not be inspected.
    Uninspectable,
    /// The kernel names no user or process for it.
    Unidentified,
    OtherUser,
    NotHub,
}

impl PeerFault {
    fn what(self) -> &'static str {
        match self {
            Self::Uninspectable => "could not be inspected",
            Self::Unidentified => "cannot be identified",
            Self::OtherUser => "belongs to another user",
            Self::NotHub => "is not the Hub",
        }
    }

    /// The refusal a command gives when the listener is not the service.
    fn for_command(self) -> OperatorRefusal {
        match self {
            Self::Uninspectable => failed(self.what()),
            Self::NotHub => unauthenticated("is not the Hub service"),
            _ => unauthenticated(self.what()),
        }
    }

    /// The refusal the service gives a requester it does not admit.
    fn for_service(self) -> OperatorRefusal {
        peer_refused(self.what())
    }
}

/// Runs `work` where a Tokio reactor is current: the caller's, or a small
/// one of its own.
fn with_reactor<T>(work: impl FnOnce() -> T) -> Result<T, OperatorRefusal> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let _entered = handle.enter();
        return Ok(work());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .map_err(|_| failed("could not be inspected"))?;
    let _entered = runtime.enter();
    Ok(work())
}

/// The peer's user id and process id, as the kernel reports them.
fn peer_identity(stream: &UnixStream) -> Result<(u32, i32), PeerFault> {
    let clone = stream.try_clone().map_err(|_| PeerFault::Uninspectable)?;
    // Tokio registers only non-blocking sockets. The clone shares the
    // stream's open file description, so blocking mode is restored after.
    clone
        .set_nonblocking(true)
        .map_err(|_| PeerFault::Uninspectable)?;
    let credentials = with_reactor(|| {
        tokio::net::UnixStream::from_std(clone).and_then(|peer| peer.peer_cred())
    });
    stream
        .set_nonblocking(false)
        .map_err(|_| PeerFault::Uninspectable)?;
    let credentials = credentials
        .map_err(|_| PeerFault::Uninspectable)?
        .map_err(|_| PeerFault::Unidentified)?;
    let pid = credentials
        .pid()
        .filter(|pid| *pid > 0)
        .ok_or(PeerFault::Unidentified)?;
    Ok((credentials.uid(), pid))
}

/// The executable a process runs, from the kernel's record of its exec path.
fn process_executable(pid: i32) -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        return std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    }
    let output = std::process::Command::new("/bin/ps")
        .args(["-ww", "-o", "comm=", "-p", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let path = text.trim();
    (output.status.success() && path.starts_with('/')).then(|| PathBuf::from(path))
}

/// The same file, or a copy with the same bytes (the app may carry its own).
fn same_executable(left: &Path, right: &Path) -> bool {
    let (Ok(a), Ok(b)) = (std::fs::metadata(left), std::fs::metadata(right)) else {
        return false;
    };
    if a.dev() == b.dev() && a.ino() == b.ino() {
        return true;
    }
    if a.len() != b.len() {
        return false;
    }
    let digest = |path: &Path| -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        Some(crate::collection::window::ledger::sha256_hex(&bytes))
    };
    matches!((digest(left), digest(right)), (Some(a), Some(b)) if a == b)
}

/// Authenticates the process at the other end of `stream` as the Hub: a
/// process of this user running this Hub executable (the same file, or a
/// copy with the same bytes). Both ends apply it: a command to the listener
/// before it reads any input (review finding 7), and the service to every
/// requester before it reads or acts on a request (M02-T11 review), so a
/// process of the same user that is not the Hub can neither receive a
/// command's input nor have the service act for it.
fn verify_peer(stream: &UnixStream) -> Result<(), PeerFault> {
    let (uid, pid) = peer_identity(stream)?;
    if uid != rustix::process::getuid().as_raw() {
        return Err(PeerFault::OtherUser);
    }
    let ours = std::env::current_exe().map_err(|_| PeerFault::Uninspectable)?;
    let theirs = process_executable(pid).ok_or(PeerFault::Unidentified)?;
    if !same_executable(&ours, &theirs) {
        return Err(PeerFault::NotHub);
    }
    Ok(())
}

/// Authenticates the listener as the admitted resident service, the only
/// process that binds the socket and only while it holds the data-directory
/// lifetime lock. A listener of any other program is refused before the
/// command reads any input (review finding 7).
fn authenticate(stream: &UnixStream) -> Result<(), OperatorRefusal> {
    verify_peer(stream).map_err(PeerFault::for_command)
}

/// Authenticates a requester as the Hub's own operator command. Anything
/// else, even a process of the owner, is refused before its request is read.
fn authenticate_requester(stream: &UnixStream) -> Result<(), OperatorRefusal> {
    verify_peer(stream).map_err(PeerFault::for_service)
}

/// Connects to the service on `data_dir`'s channel and authenticates it.
/// Reads nothing else: the command reads its input only after this, and
/// sends it on this same connection.
pub fn connect(data_dir: &Path) -> Result<UnixStream, OperatorRefusal> {
    let path = socket_path(data_dir);
    if !check_private_dir(&channel_dir(data_dir))? {
        return Err(OperatorRefusal::service_not_running());
    }
    let stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(OperatorRefusal::service_not_running());
        }
        Err(_) => return Err(failed("could not be reached")),
    };
    authenticate(&stream)?;
    Ok(stream)
}

/// Sends one request on a connected stream and returns the service's result
/// or its refusal.
pub fn exchange(
    mut stream: UnixStream,
    request: &OperatorRequest,
    timeout: Duration,
) -> Result<serde_json::Value, OperatorRefusal> {
    let mut bytes = serde_json::to_vec(request).map_err(|_| failed("request is not encodable"))?;
    if bytes.len() > REQUEST_MAX_BYTES {
        bytes.zeroize();
        return Err(failed("request is too large"));
    }
    bytes.push(b'\n');
    let _ = stream.set_write_timeout(Some(timeout));
    let _ = stream.set_read_timeout(Some(timeout));
    let written = stream.write_all(&bytes);
    bytes.zeroize();
    written.map_err(|_| failed("request could not be sent"))?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut answer = Vec::new();
    (&mut stream)
        .take(RESPONSE_MAX_BYTES as u64 + 1)
        .read_to_end(&mut answer)
        .map_err(|_| failed("answer did not arrive"))?;
    if answer.len() > RESPONSE_MAX_BYTES {
        return Err(failed("answer is too large"));
    }
    let response: OperatorResponse =
        serde_json::from_slice(&answer).map_err(|_| failed("answer is malformed"))?;
    if response.ok {
        Ok(response.result.unwrap_or(serde_json::Value::Null))
    } else {
        Err(OperatorRefusal::new(
            response.code.as_deref().unwrap_or(CHANNEL_FAILED),
            response.message.unwrap_or_default(),
        ))
    }
}

/// Connects, authenticates the service and sends one request.
pub fn forward(
    data_dir: &Path,
    request: &OperatorRequest,
    timeout: Duration,
) -> Result<serde_json::Value, OperatorRefusal> {
    exchange(connect(data_dir)?, request, timeout)
}

/// The listening side. One thread accepts; each request is served on a
/// worker of its own, at most [`MAX_ACTIVE_REQUESTS`] at once, and must
/// arrive whole within its deadline. Dropping it stops accepting, shuts
/// down every active connection, waits briefly for the workers and removes
/// the socket.
pub struct ChannelServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    active: Arc<Active>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for ChannelServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelServer").field("path", &self.path).finish()
    }
}

/// The connections being served, so a stop can interrupt them.
#[derive(Default)]
struct Active {
    streams: Mutex<Vec<(u64, UnixStream)>>,
    idle: Condvar,
    next: std::sync::atomic::AtomicU64,
}

impl Active {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(u64, UnixStream)>> {
        self.streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers a connection, or `None` when the service is at capacity.
    fn enter(&self, stream: &UnixStream) -> Option<u64> {
        let mut streams = self.lock();
        if streams.len() >= MAX_ACTIVE_REQUESTS {
            return None;
        }
        let clone = stream.try_clone().ok()?;
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        streams.push((id, clone));
        Some(id)
    }

    fn leave(&self, id: u64) {
        let mut streams = self.lock();
        streams.retain(|(other, _)| *other != id);
        if streams.is_empty() {
            self.idle.notify_all();
        }
    }

    /// Shuts down every active connection, so blocked reads and writes end,
    /// and waits up to `wait` for their workers to leave.
    fn interrupt(&self, wait: Duration) {
        let mut streams = self.lock();
        for (_, stream) in streams.iter() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        let deadline = Instant::now() + wait;
        while !streams.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            streams = self
                .idle
                .wait_timeout(streams, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

/// Reads one request, refusing it once `deadline` passes however the bytes
/// trickle in.
fn read_request(stream: &mut UnixStream, deadline: Instant) -> Result<OperatorRequest, OperatorRefusal> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    let result = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break Err(failed("request did not arrive in time"));
        }
        if stream.set_read_timeout(Some(left)).is_err() {
            break Err(failed("request did not arrive"));
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                break serde_json::from_slice::<OperatorRequest>(&bytes).map_err(|_| {
                    OperatorRefusal::new("request_invalid", "the request is not understood")
                });
            }
            Ok(read) => {
                bytes.extend_from_slice(&chunk[..read]);
                if bytes.len() > REQUEST_MAX_BYTES {
                    break Err(failed("request is too large"));
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                break Err(failed("request did not arrive in time"));
            }
            Err(_) => break Err(failed("request did not arrive")),
        }
    };
    chunk.zeroize();
    bytes.zeroize();
    result
}

fn answer(stream: &mut UnixStream, result: Result<serde_json::Value, OperatorRefusal>) {
    let response = OperatorResponse::from_result(result);
    if let Ok(mut bytes) = serde_json::to_vec(&response) {
        bytes.push(b'\n');
        let _ = stream.set_write_timeout(Some(REQUEST_DEADLINE));
        let _ = stream.write_all(&bytes);
    }
}

fn serve_one(mut stream: UnixStream, handler: &dyn OperatorHandler, deadline: Duration) {
    let deadline = Instant::now() + deadline;
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    // The requester is authenticated before a byte of its request is read,
    // for every request: an unadmitted process gets a refusal and nothing
    // reaches the handler.
    let result = authenticate_requester(&stream)
        .and_then(|()| read_request(&mut stream, deadline))
        .and_then(|request| handler.handle(request));
    answer(&mut stream, result);
}

impl ChannelServer {
    /// Binds the socket for `data_dir`. The caller holds the data-directory
    /// lifetime lock, so a socket left by an earlier process is stale.
    pub fn bind(data_dir: &Path, handler: Arc<dyn OperatorHandler>) -> std::io::Result<Self> {
        Self::bind_with_deadline(data_dir, handler, REQUEST_DEADLINE)
    }

    /// [`Self::bind`] with another request deadline (tests shorten it).
    pub fn bind_with_deadline(
        data_dir: &Path,
        handler: Arc<dyn OperatorHandler>,
        deadline: Duration,
    ) -> std::io::Result<Self> {
        let dir = channel_dir(data_dir);
        let path = socket_path(data_dir);
        if path.as_os_str().len() > SOCKET_PATH_MAX {
            return Err(std::io::Error::other(
                "control channel path is too long for a Unix socket",
            ));
        }
        match check_private_dir(&dir) {
            Ok(true) => {}
            Ok(false) => {
                std::fs::create_dir(&dir)?;
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            }
            Err(refusal) => return Err(std::io::Error::other(refusal.to_string())),
        }
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if std::os::unix::fs::FileTypeExt::is_socket(&metadata.file_type()) => {
                std::fs::remove_file(&path)?;
            }
            Ok(_) => {
                return Err(std::io::Error::other(
                    "control channel path holds something other than a socket",
                ));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(Active::default());
        let thread = {
            let stop = stop.clone();
            let active = active.clone();
            std::thread::Builder::new()
                .name("hub-operator-channel".to_owned())
                .spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        match listener.accept() {
                            Ok((mut stream, _)) => {
                                let _ = stream.set_nonblocking(false);
                                let Some(id) = active.enter(&stream) else {
                                    answer(
                                        &mut stream,
                                        Err(OperatorRefusal::new(
                                            CHANNEL_BUSY,
                                            "the service is answering other requests; try again",
                                        )),
                                    );
                                    continue;
                                };
                                let handler = handler.clone();
                                let worker_active = active.clone();
                                let spawned = std::thread::Builder::new()
                                    .name("hub-operator-request".to_owned())
                                    .spawn(move || {
                                        serve_one(stream, handler.as_ref(), deadline);
                                        worker_active.leave(id);
                                    });
                                if spawned.is_err() {
                                    active.leave(id);
                                }
                            }
                            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                                std::thread::sleep(ACCEPT_POLL);
                            }
                            Err(_) => std::thread::sleep(ACCEPT_POLL),
                        }
                    }
                })?
        };
        Ok(Self {
            path,
            stop,
            active,
            thread: Some(thread),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ChannelServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Active requests are interrupted, not awaited: a stop never waits
        // on a client. A handler still running after the wait is detached.
        self.active.interrupt(SHUTDOWN_WAIT);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether `refusal` says no service answered.
pub fn is_not_running(refusal: &OperatorRefusal) -> bool {
    refusal.code == SERVICE_NOT_RUNNING
}
