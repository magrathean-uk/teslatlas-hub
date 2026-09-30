// SPDX-License-Identifier: AGPL-3.0-only
//! Closed, local development evidence. Never accepts free-form payloads.
use rustix::fs::{AtFlags, Mode, OFlags, open, openat, renameat, unlinkat};
use serde::Serialize;
use std::{
    fs::File,
    io::{self, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const DIRECTORY_ENV: &str = "TESLATLAS_HUB_DEVELOPMENT_LOG_DIRECTORY";
const SEGMENT_BYTES: u64 = 10 * 1024 * 1024;
const MAX_AGE: Duration = Duration::from_secs(7 * 86400);
static JOURNAL: OnceLock<Option<Journal>> = OnceLock::new();
tokio::task_local! { static REQUEST: Uuid; }
#[cfg(test)]
tokio::task_local! { static TEST_JOURNAL: Journal; }

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Http,
    Sync,
    Import,
    Publication,
    CapacityFallback,
    Command,
    Current,
    Serve,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Start,
    Progress,
    Complete,
    Failed,
    NoOp,
    ChangedSet,
    RebaseRequired,
    FullBase,
    FullReplacement,
    Stopped,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Counting,
    Metadata,
    RelatedPositions,
    Positions,
    Charges,
    Finalizing,
    Complete,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Endpoint {
    Health,
    Readiness,
    Discovery,
    ChangesSince,
    Manifest,
    Pack,
    PreparedMap,
    Drives,
    Charges,
    Other,
}
impl Endpoint {
    pub fn from_route(route: &str) -> Self {
        match route {
            "/health" | "/healthz" => Self::Health,
            "/ready" | "/readyz" => Self::Readiness,
            "/v1/discovery" | "/.well-known/teslatlas-hub" => Self::Discovery,
            "/v1/vehicles/{vehicle_id}/sync/changes-since" => Self::ChangesSince,
            "/v1/vehicles/{vehicle_id}/sync/manifest" => Self::Manifest,
            "/v1/packs/sha256/{object_name}" => Self::Pack,
            "/v1/vehicles/{vehicle_id}/prepared-map"
            | "/v1/vehicles/{vehicle_id}/sync/prepared-artefacts/{artifact_id}" => {
                Self::PreparedMap
            }
            "/v1/vehicles/{vehicle_id}/drives" => Self::Drives,
            "/v1/vehicles/{vehicle_id}/charges" => Self::Charges,
            _ => Self::Other,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Get,
    Post,
    Other,
}
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub kind: Kind,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<Endpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<Method>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_sequence: Option<u64>,
    pub duration_ms: u64,
    pub rows: u64,
    pub total_rows: u64,
    pub chunks: u64,
}
impl Event {
    pub fn new(kind: Kind, outcome: Outcome) -> Self {
        Self {
            kind,
            outcome,
            phase: None,
            endpoint: None,
            method: None,
            status: None,
            base_sequence: None,
            target_sequence: None,
            duration_ms: 0,
            rows: 0,
            total_rows: 0,
            chunks: 0,
        }
    }
}
#[derive(Serialize)]
struct Frame {
    schema: u8,
    utc_ms: u64,
    session: Uuid,
    pid: u32,
    request: Option<Uuid>,
    dropped_total: u64,
    #[serde(flatten)]
    event: Event,
}
enum Message {
    Event(Frame),
    Flush(mpsc::Sender<bool>),
}
struct Journal {
    sender: SyncSender<Message>,
    dropped: std::sync::Arc<AtomicU64>,
    session: Uuid,
}
pub fn enabled() -> bool {
    #[cfg(test)]
    if TEST_JOURNAL.try_with(|_| ()).is_ok() {
        return true;
    }
    JOURNAL.get().is_some_and(Option::is_some)
}
pub fn development_requested() -> bool {
    std::env::var("TESLATLAS_HUB_DEVELOPMENT").as_deref() == Ok("1")
}
pub fn initialize() -> io::Result<bool> {
    if !development_requested() {
        let _ = JOURNAL.set(None);
        return Ok(false);
    }
    let Some(path) = std::env::var_os(DIRECTORY_ENV) else {
        let _ = JOURNAL.set(None);
        return Ok(false);
    };
    let journal = Journal::open(PathBuf::from(path), SEGMENT_BYTES)?;
    let _ = JOURNAL.set(Some(journal));
    Ok(true)
}
pub fn record(event: Event) {
    #[cfg(test)]
    if TEST_JOURNAL
        .try_with(|j| j.record(event.clone(), REQUEST.try_with(|id| *id).ok()))
        .is_ok()
    {
        return;
    }
    if let Some(Some(j)) = JOURNAL.get() {
        j.record(event, REQUEST.try_with(|id| *id).ok());
    }
}
#[cfg(test)]
pub(crate) async fn with_test_journal<T>(
    path: PathBuf,
    future: impl std::future::Future<Output = T>,
) -> T {
    TEST_JOURNAL
        .scope(Journal::open(path, SEGMENT_BYTES).unwrap(), async move {
            let result = future.await;
            assert!(TEST_JOURNAL.with(|j| j.flush(Duration::from_secs(5))));
            result
        })
        .await
}
pub async fn request_scope<T>(id: Uuid, future: impl std::future::Future<Output = T>) -> T {
    REQUEST.scope(id, future).await
}
pub fn flush() -> bool {
    JOURNAL
        .get()
        .and_then(Option::as_ref)
        .is_none_or(|j| j.flush(Duration::from_millis(500)))
}
impl Journal {
    fn open(path: PathBuf, limit: u64) -> io::Result<Self> {
        let directory = validated_directory(&path)?;
        let lock = owned_file(&directory, ".hub-events.lock")?;
        let (sender, receiver) = mpsc::sync_channel(64);
        let dropped = std::sync::Arc::new(AtomicU64::new(0));
        let lost = dropped.clone();
        std::thread::Builder::new()
            .name("hub-dev-journal".into())
            .spawn(move || {
                for message in receiver {
                    match message {
                        Message::Event(mut frame) => {
                            frame.dropped_total = lost.load(Ordering::Relaxed);
                            let result = (|| {
                                lock.lock()?;
                                let result = append(&directory, &frame, limit);
                                let unlocked = lock.unlock();
                                result.and(unlocked)
                            })();
                            if result.is_err() {
                                lost.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Message::Flush(done) => {
                            let _ = done.send(lost.load(Ordering::Relaxed) == 0);
                        }
                    }
                }
            })?;
        Ok(Self {
            sender,
            dropped,
            session: Uuid::new_v4(),
        })
    }
    fn record(&self, event: Event, request: Option<Uuid>) {
        let frame = Frame {
            schema: 1,
            utc_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64,
            session: self.session,
            pid: std::process::id(),
            request,
            dropped_total: 0,
            event,
        };
        if self.sender.try_send(Message::Event(frame)).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn flush(&self, timeout: Duration) -> bool {
        let (tx, rx) = mpsc::channel();
        self.sender.try_send(Message::Flush(tx)).is_ok()
            && rx.recv_timeout(timeout).unwrap_or(false)
    }
}
pub(crate) fn validated_directory(path: &Path) -> io::Result<File> {
    validated_directory_with_hook(path, |_| {})
}
fn validated_directory_with_hook(
    path: &Path,
    mut after_open: impl FnMut(&Path),
) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = File::from(open("/", flags, Mode::empty())?);
    let mut opened = PathBuf::from("/");
    let validate = |directory: &File, leaf: bool| -> io::Result<()> {
        let metadata = directory.metadata()?;
        let uid = rustix::process::getuid().as_raw();
        let gid = rustix::process::getgid().as_raw();
        if !metadata.is_dir()
            || (metadata.uid() != 0 && metadata.uid() != uid)
            || metadata.mode() & 0o002 != 0
            || (metadata.mode() & 0o020 != 0 && (metadata.uid() != uid || metadata.gid() != gid))
            || (leaf && (metadata.uid() != uid || metadata.mode() & 0o777 != 0o700))
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    };
    validate(&directory, path == Path::new("/"))?;
    let mut components = path.components().skip(1).peekable();
    while let Some(component) = components.next() {
        let std::path::Component::Normal(name) = component else {
            return Err(io::ErrorKind::PermissionDenied.into());
        };
        directory = File::from(openat(&directory, name, flags, Mode::empty())?);
        opened.push(name);
        validate(&directory, components.peek().is_none())?;
        after_open(&opened);
    }
    Ok(directory)
}
fn owned_file(dir: &File, name: &str) -> io::Result<File> {
    let fd = openat(
        dir,
        name,
        OFlags::RDWR
            | OFlags::CREATE
            | OFlags::APPEND
            | OFlags::NOFOLLOW
            | OFlags::CLOEXEC
            | OFlags::NONBLOCK,
        Mode::RUSR | Mode::WUSR,
    )?;
    let f = File::from(fd);
    let m = f.metadata()?;
    if !m.is_file()
        || m.uid() != rustix::process::getuid().as_raw()
        || m.mode() & 0o777 != 0o600
        || m.nlink() != 1
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(f)
}
fn segment(i: usize) -> String {
    format!("hub-events.{i}.jsonl")
}
fn append(dir: &File, frame: &Frame, limit: u64) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(frame)?;
    bytes.push(b'\n');
    for i in 0..5 {
        let name = segment(i);
        match openat(
            dir,
            name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(fd) => {
                let f = File::from(fd);
                let m = f.metadata()?;
                if !m.is_file()
                    || m.uid() != rustix::process::getuid().as_raw()
                    || m.mode() & 0o777 != 0o600
                    || m.nlink() != 1
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                if m.modified()?.elapsed().unwrap_or_default() > MAX_AGE {
                    unlinkat(dir, name.as_str(), AtFlags::empty())?;
                }
            }
            Err(rustix::io::Errno::NOENT) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let current = owned_file(dir, &segment(0))?;
    let rotate = current.metadata()?.len() + bytes.len() as u64 > limit;
    drop(current);
    if rotate {
        match unlinkat(dir, segment(4).as_str(), AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => {}
            Err(e) => return Err(e.into()),
        }
        for i in (0..4).rev() {
            match renameat(dir, segment(i).as_str(), dir, segment(i + 1).as_str()) {
                Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    let mut f = owned_file(dir, &segment(0))?;
    f.write_all(&bytes)?;
    f.sync_data()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn development_journal_walk_stays_anchored_during_ancestor_swap() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let t = crate::private_tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let ancestor = root.join("ancestor");
        let held = root.join("held");
        let redirected = root.join("redirected");
        for path in [ancestor.join("logs"), redirected.join("logs")] {
            std::fs::create_dir_all(&path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o775)).unwrap();
        let directory = validated_directory_with_hook(&ancestor.join("logs"), |opened| {
            if opened == ancestor {
                std::fs::rename(&ancestor, &held).unwrap();
                symlink(&redirected, &ancestor).unwrap();
            }
        })
        .unwrap();
        owned_file(&directory, "safe-marker").unwrap();
        assert!(held.join("logs/safe-marker").is_file());
        assert!(!redirected.join("logs/safe-marker").exists());
        assert!(validated_directory(&ancestor.join("logs")).is_err());
    }
    #[test]
    fn development_journal_walk_rejects_unopened_child_swap() {
        use std::os::unix::fs::symlink;
        let t = crate::private_tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let logs = root.join("logs");
        std::fs::create_dir(&logs).unwrap();
        let result = validated_directory_with_hook(&logs, |opened| {
            if opened == root {
                std::fs::rename(&logs, root.join("held")).unwrap();
                symlink(root.join("held"), &logs).unwrap();
            }
        });
        assert!(result.is_err());
    }
    #[test]
    fn development_journal_rotates_and_has_closed_payload() {
        let t = crate::private_tempdir().unwrap();
        let p = t.path().canonicalize().unwrap();
        let j = Journal::open(p.clone(), 450).unwrap();
        for _ in 0..10 {
            j.record(Event::new(Kind::Import, Outcome::Complete), None);
        }
        assert!(j.flush(Duration::from_secs(5)));
        let files: Vec<_> = std::fs::read_dir(&p)
            .unwrap()
            .map(|x| x.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        assert!(files.len() <= 5);
        for p in files {
            assert_eq!(p.metadata().unwrap().mode() & 0o777, 0o600);
            let content = std::fs::read_to_string(p).unwrap();
            for line in content.lines() {
                let v: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_eq!(v["kind"], "import");
                assert!(v.get("uri").is_none());
                assert!(v.get("error").is_none());
                assert!(v.get("source").is_none());
            }
        }
        assert!(!enabled());
        assert_eq!(
            Endpoint::from_route("/.well-known/teslatlas-hub"),
            Endpoint::Discovery
        );
        assert_eq!(
            Endpoint::from_route("/v1/vehicles/{vehicle_id}/sync/prepared-artefacts/{artifact_id}"),
            Endpoint::PreparedMap
        );
        assert_eq!(
            Endpoint::from_route("/v1/vehicles/private-id?token=secret"),
            Endpoint::Other
        );
    }
    #[test]
    fn development_journal_accepts_owner_directory_read_only() {
        if let Some(path) = std::env::var_os("TESLATLAS_HUB_LOG_POLICY_DIRECTORY") {
            let directory = validated_directory(Path::new(&path)).unwrap();
            assert!(directory.metadata().unwrap().is_dir());
        } else {
            let temp = crate::private_tempdir().unwrap();
            assert!(validated_directory(&temp.path().canonicalize().unwrap()).is_ok());
        }
    }
    #[test]
    fn development_journal_rejects_symlinks_and_unsafe_directory() {
        let t = crate::private_tempdir().unwrap();
        let p = t.path().canonicalize().unwrap();
        std::os::unix::fs::symlink("/dev/null", p.join("hub-events.0.jsonl")).unwrap();
        let dir = validated_directory(&p).unwrap();
        let f = Frame {
            schema: 1,
            utc_ms: 0,
            session: Uuid::new_v4(),
            pid: 1,
            request: None,
            dropped_total: 0,
            event: Event::new(Kind::Command, Outcome::Start),
        };
        assert!(append(&dir, &f, SEGMENT_BYTES).is_err());
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validated_directory(&p).is_err());
    }
}
