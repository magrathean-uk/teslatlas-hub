// SPDX-License-Identifier: AGPL-3.0-only

//! The local control channel between operator commands and the running
//! service (ruling R5).
//!
//! The service binds one Unix socket inside a private directory of its data
//! directory and answers one bounded JSON request per connection. A command
//! connects before it reads a secret path or standard input: no socket, or no
//! listener behind it, is `service_not_running`, so a refused command has read
//! nothing and opened no network connection. Requests carry bounded material
//! only (a grant, a trust document, the sign-in code material, recovery file
//! paths); no request carries an access or refresh token.

use std::{
    fmt,
    io::{ErrorKind, Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use super::admission::{OperatorRefusal, SERVICE_NOT_RUNNING};

/// Largest request the service reads (a 4 KiB grant fits encoded).
pub const REQUEST_MAX_BYTES: usize = 16 * 1024;
/// Largest response a command reads.
pub const RESPONSE_MAX_BYTES: usize = 256 * 1024;
/// How long the service waits for a request to arrive.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Accept poll interval of the service thread.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// Longest socket path macOS accepts, less its terminator.
const SOCKET_PATH_MAX: usize = 103;

pub const CHANNEL_UNSAFE: &str = "control_channel_unsafe";
pub const CHANNEL_FAILED: &str = "control_channel_failed";

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

/// Whether a service answers on `data_dir`'s channel. Reads nothing else.
pub fn connect(data_dir: &Path) -> Result<UnixStream, OperatorRefusal> {
    let path = socket_path(data_dir);
    if !check_private_dir(&channel_dir(data_dir))? {
        return Err(OperatorRefusal::service_not_running());
    }
    match UnixStream::connect(&path) {
        Ok(stream) => Ok(stream),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            Err(OperatorRefusal::service_not_running())
        }
        Err(_) => Err(failed("could not be reached")),
    }
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

/// Connects and sends one request.
pub fn forward(
    data_dir: &Path,
    request: &OperatorRequest,
    timeout: Duration,
) -> Result<serde_json::Value, OperatorRefusal> {
    exchange(connect(data_dir)?, request, timeout)
}

/// The listening side, served by one thread; requests are handled one at a
/// time. Dropping it stops the thread and removes the socket.
pub struct ChannelServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for ChannelServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelServer").field("path", &self.path).finish()
    }
}

fn read_request(stream: &mut UnixStream) -> Result<OperatorRequest, OperatorRefusal> {
    let _ = stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT));
    let mut bytes = Vec::new();
    let read = stream
        .take(REQUEST_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes);
    let result = match read {
        Err(_) => Err(failed("request did not arrive")),
        Ok(_) if bytes.len() > REQUEST_MAX_BYTES => Err(failed("request is too large")),
        Ok(_) => serde_json::from_slice::<OperatorRequest>(&bytes)
            .map_err(|_| OperatorRefusal::new("request_invalid", "the request is not understood")),
    };
    bytes.zeroize();
    result
}

fn serve_one(mut stream: UnixStream, handler: &dyn OperatorHandler) {
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let result = read_request(&mut stream).and_then(|request| handler.handle(request));
    let response = OperatorResponse::from_result(result);
    if let Ok(mut bytes) = serde_json::to_vec(&response) {
        bytes.push(b'\n');
        let _ = stream.set_write_timeout(Some(REQUEST_READ_TIMEOUT));
        let _ = stream.write_all(&bytes);
    }
}

impl ChannelServer {
    /// Binds the socket for `data_dir`. The caller holds the data-directory
    /// lifetime lock, so a socket left by an earlier process is stale.
    pub fn bind(data_dir: &Path, handler: Arc<dyn OperatorHandler>) -> std::io::Result<Self> {
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
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("hub-operator-channel".to_owned())
                .spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        match listener.accept() {
                            Ok((stream, _)) => serve_one(stream, handler.as_ref()),
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
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether `refusal` says no service answered.
pub fn is_not_running(refusal: &OperatorRefusal) -> bool {
    refusal.code == SERVICE_NOT_RUNNING
}
