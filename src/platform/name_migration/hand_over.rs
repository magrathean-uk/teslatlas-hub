// SPDX-License-Identifier: AGPL-3.0-only

//! The installed Hub's hand-over to its current names on a Mac (DEC-160).
//!
//! The app runs this before it reads any Hub path, in two steps:
//!
//! 1. [`prepare`]: a hand-over note is written into the earlier data folder
//!    (so it moves with it), the earlier LaunchAgent is booted out if loaded
//!    (two services must never share a host), the window claim folder moves
//!    (a host-wide fence: a claim still held under the earlier name defers
//!    everything), the data folder moves, the configuration is pointed at
//!    it, the logs and caches follow, and the earlier label is disabled so a
//!    login cannot load it against moved data. If the data cannot move, the
//!    earlier service is started again and nothing else changes; the note is
//!    kept until that restart has worked, so a failed restart is tried again
//!    by the next attempt or the next launch.
//! 2. The app writes the current LaunchAgent property list (its own writer),
//!    then, if the earlier service was running, [`complete`] confirms that the
//!    current one serves its configuration, starting it only when it does not
//!    (see `cli`). Only then is the earlier property list removed and the note
//!    deleted.
//!
//! Every step is idempotent and resumes from the note after a crash: a run
//! that finds the note in the current folder repeats steps 1 and 2 from where
//! they stopped. A Hub that the owner had stopped stays stopped (its disabled
//! state carries over to the current label). A current Hub that was already
//! installed keeps its own service state: the earlier one is stopped and
//! retired, and nothing is disabled or started for the current one. Before
//! the rename the earlier and current names are the same and nothing here runs.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use super::{
    config_paths::{self, ConfigOutcome},
    folders::{self, FolderMove, FolderOutcome},
    spelling,
};

/// The LaunchAgent label; the same value as the service module's own.
pub const LAUNCH_AGENT_LABEL: &str = "com.tesatlas.hub";
/// The Hub folder in `~/Library/Application Support`.
pub const DATA_FOLDER: &str = "Tesatlas Hub";
/// The Hub log folder, relative to the home folder (the agent's own logs).
pub const LOG_FOLDER: &str = "Library/Logs/Tesatlas Hub";
/// The terrain cache folder in `~/Library/Caches` (see `TerrainConfig`).
pub const TERRAIN_CACHE_FOLDER: &str = "TesatlasHub";
/// The app's cache folder in `~/Library/Caches` (its bundle identifier).
pub const APP_CACHE_FOLDER: &str = "eu.tesatlas.hub.app";
/// The lifetime lock of a serving Hub, relative to the data folder.
pub const LIFETIME_LOCK: &str = "data/.hub-instance.lock";
/// The window host claim, relative to its folder.
pub const HOST_CLAIM_LOCK: &str = "host-claim.lock";
/// The hand-over note; it moves with the data folder.
pub const NOTE: &str = "rename-hand-over.json";
const NO_LOCKS: &[&str] = &[];
const HOST_CLAIM_LOCKS: &[&str] = &[HOST_CLAIM_LOCK];

/// How long a booted-out earlier service may take to exit (its ExitTimeOut is 270 s).
#[cfg(not(test))]
const UNLOAD_DEADLINE: Duration = Duration::from_secs(300);
#[cfg(test)]
const UNLOAD_DEADLINE: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const UNLOAD_POLL: Duration = Duration::from_millis(100);
#[cfg(test)]
const UNLOAD_POLL: Duration = Duration::ZERO;

/// launchd as the hand-over uses it; the real one runs `/bin/launchctl`.
pub trait Launchd {
    fn loaded(&mut self, service: &str) -> io::Result<bool>;
    fn disabled(&mut self, domain: &str, label: &str) -> io::Result<bool>;
    fn bootout(&mut self, service: &str) -> io::Result<()>;
    fn bootstrap(&mut self, domain: &str, plist: &Path) -> io::Result<()>;
    fn disable(&mut self, service: &str) -> io::Result<()>;
}

/// The owner's folders, launchd domain and the current names.
#[derive(Debug, Clone)]
pub struct Layout {
    pub home: PathBuf,
    /// `gui/<uid>`.
    pub domain: String,
    /// The current LaunchAgent label.
    pub label: String,
    /// The current folder name in `~/Library/Application Support`.
    pub data_folder: String,
    /// Folders that follow the data folder: a name for the report, the
    /// current path relative to the home folder, and the lock files in it. A
    /// folder with a lock (the window claim) is a fence: it must move before
    /// the data does. The others (logs, caches) follow on a best-effort basis.
    pub other_folders: Vec<(&'static str, PathBuf, &'static [&'static str])>,
}

impl Layout {
    /// The installed Hub's names.
    pub fn production(home: PathBuf, domain: String) -> Self {
        let window = crate::collection::window::HostClaim::default_path(Path::new(""));
        let window = window.parent().map(Path::to_path_buf).unwrap_or_default();
        Self {
            home,
            domain,
            label: LAUNCH_AGENT_LABEL.to_owned(),
            data_folder: DATA_FOLDER.to_owned(),
            other_folders: vec![
                ("window", window, HOST_CLAIM_LOCKS),
                ("logs", PathBuf::from(LOG_FOLDER), NO_LOCKS),
                ("terrain_cache", Path::new("Library/Caches").join(TERRAIN_CACHE_FOLDER), NO_LOCKS),
                ("app_cache", Path::new("Library/Caches").join(APP_CACHE_FOLDER), NO_LOCKS),
            ],
        }
    }

    fn library(&self) -> PathBuf {
        self.home.join("Library")
    }
    pub fn data_root(&self) -> PathBuf {
        self.library().join("Application Support").join(&self.data_folder)
    }
    fn plist(&self, label: &str) -> PathBuf {
        self.library().join("LaunchAgents").join(format!("{label}.plist"))
    }
    fn service(&self, label: &str) -> String {
        format!("{}/{label}", self.domain)
    }
    fn data_move(&self) -> Option<FolderMove> {
        FolderMove::for_current(self.data_root(), &[LIFETIME_LOCK])
    }
    /// The folders that follow the data folder, each with whether it is a
    /// fence (it holds a lock, so its move is required).
    fn other_moves(&self) -> Vec<(&'static str, Option<FolderMove>, bool)> {
        self.other_folders
            .iter()
            .map(|(name, path, locks)| (*name, FolderMove::for_current(self.home.join(path), locks), !locks.is_empty()))
            .collect()
    }
}

/// What the earlier service was doing when the hand-over began, and whether
/// a current installation already existed then.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Note {
    pub service_was_loaded: bool,
    pub service_was_disabled: bool,
    /// The current LaunchAgent was already installed: its own service state
    /// is kept, never replaced by the earlier service's.
    #[serde(default)]
    pub current_was_installed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Before the rename: nothing to do.
    Inert,
    /// No earlier installation to hand over.
    Nothing,
    /// The data is under the current name; the app installs the current agent next.
    Prepared,
    /// The hand-over finished.
    Done,
    /// A process still holds the earlier folder; nothing changed, try again.
    Deferred,
    /// A step failed; see `detail`. Nothing was lost; try again.
    Failed,
}

/// The step's report, printed as JSON for the app.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub status: Status,
    /// The app must write the current LaunchAgent property list and run `complete`.
    pub install: bool,
    /// `complete` must start the current service (the earlier one was running).
    pub start: bool,
    pub data: Option<FolderOutcome>,
    pub config: Option<ConfigOutcome>,
    pub folders: Vec<(String, String)>,
    pub detail: Option<String>,
}

impl Report {
    fn new(status: Status) -> Self {
        Self { status, install: false, start: false, data: None, config: None, folders: Vec::new(), detail: None }
    }
    fn failed(detail: impl std::fmt::Display) -> Self {
        Self { detail: Some(detail.to_string()), ..Self::new(Status::Failed) }
    }
    pub(super) fn failed_with(detail: &str) -> Self {
        Self::failed(detail)
    }
    /// A development source run, or a system without LaunchAgents.
    pub(super) fn skipped() -> Self {
        Self::new(Status::Inert)
    }
}

/// Step 1 (see the module notes).
pub fn prepare(layout: &Layout, launchd: &mut dyn Launchd) -> Report {
    match prepare_inner(layout, launchd) {
        Ok(report) => report,
        Err(error) => Report::failed(error),
    }
}

fn prepare_inner(layout: &Layout, launchd: &mut dyn Launchd) -> io::Result<Report> {
    let (Some(earlier_label), Some(data)) = (spelling::earlier(&layout.label), layout.data_move()) else {
        return Ok(Report::new(Status::Inert));
    };
    let earlier_service = layout.service(&earlier_label);
    let resumed = read_note(&data.current)?;
    let earlier_data = is_folder(&data.earlier)?;
    let earlier_plist = layout.plist(&earlier_label).is_file();
    // An earlier folder left beside a current Hub after an earlier hand-over
    // (both kept) is not handed over again: the current one is in use.
    let orphaned = earlier_data
        && !earlier_plist
        && folder_has_content(&data.current)?
        && !launchd.loaded(&earlier_service)?;
    if resumed.is_none() && (!earlier_data || orphaned) {
        let mut report = finish_without_hand_over(layout, launchd, &earlier_label)?;
        report.data = orphaned.then_some(FolderOutcome::BothKept);
        return Ok(report);
    }

    // The note records the state once, before anything changes it.
    let earlier_note = if earlier_data { read_note(&data.earlier)? } else { None };
    let note = match resumed.or(earlier_note) {
        Some(note) => note,
        None => {
            let note = Note {
                service_was_loaded: launchd.loaded(&earlier_service)?,
                // Only a live earlier installation's stopped state carries over.
                service_was_disabled: earlier_plist && launchd.disabled(&layout.domain, &earlier_label)?,
                current_was_installed: layout.plist(&layout.label).is_file(),
            };
            let holder = if folder_has_content(&data.current)? { &data.current } else { &data.earlier };
            write_note(holder, &note)?;
            note
        }
    };

    if launchd.loaded(&earlier_service)? {
        launchd.bootout(&earlier_service)?;
        if let Err(error) = wait_unloaded(launchd, &earlier_service) {
            return Ok(restore_earlier(layout, launchd, &earlier_label, &data, note, Report::failed(error)));
        }
    }

    // The window claim is a host-wide fence: it moves (or its earlier lock is
    // shown free) before the data, so no service ever starts beside a live
    // claim under the earlier name.
    let mut moved = match move_fences(layout) {
        Ok(moved) => moved,
        Err(blocked) => return Ok(restore_earlier(layout, launchd, &earlier_label, &data, note, blocked)),
    };

    let outcome = match folders::move_folder(&data) {
        Ok(outcome) => outcome,
        Err(error) => {
            let report = Report::failed(format!("the Hub folder could not be moved: {error}"));
            return Ok(restore_earlier(layout, launchd, &earlier_label, &data, note, report));
        }
    };
    match outcome {
        FolderOutcome::Deferred | FolderOutcome::NotAFolder => {
            let mut report = Report::new(if outcome == FolderOutcome::Deferred { Status::Deferred } else { Status::Failed });
            report.data = Some(outcome);
            return Ok(restore_earlier(layout, launchd, &earlier_label, &data, note, report));
        }
        FolderOutcome::Unchanged | FolderOutcome::Absent | FolderOutcome::AlreadyMoved
        | FolderOutcome::Moved | FolderOutcome::BothKept => {}
    }

    let mut report = Report::new(Status::Prepared);
    report.data = Some(outcome);
    report.config = Some(config_paths::rewrite_moved_root(
        &data.current.join("config.toml"),
        &data.earlier,
        &data.current,
    )?);
    moved.extend(move_best_effort_folders(layout));
    report.folders = moved;
    if earlier_plist {
        launchd.disable(&earlier_service)?;
    }
    // A current installation keeps its own service state; only a replacement
    // takes the earlier service's state.
    let replacing = !note.current_was_installed;
    if replacing && note.service_was_disabled {
        launchd.disable(&layout.service(&layout.label))?;
    }
    // Only an installed earlier service is installed again; the note goes
    // here when there is nothing for the app to install.
    report.install = earlier_plist || layout.plist(&layout.label).is_file();
    report.start = replacing && note.service_was_loaded && !note.service_was_disabled;
    if !report.install {
        remove_if_present(&data.current.join(NOTE))?;
    }
    Ok(report)
}

/// Step 2 (see the module notes): `start` confirms the current service serves
/// its configuration, starting it when it is not; the earlier property list
/// and the note are removed only after.
pub fn complete(
    layout: &Layout,
    start_service: bool,
    start: impl FnOnce() -> io::Result<()>,
) -> Report {
    let Some(earlier_label) = spelling::earlier(&layout.label) else { return Report::new(Status::Inert) };
    if !layout.plist(&layout.label).is_file() {
        return Report::failed("the current Hub LaunchAgent is not installed yet");
    }
    if start_service && let Err(error) = start() {
        return Report::failed(format!("the Hub service did not start under its current name: {error}"));
    }
    let retired = remove_if_present(&layout.plist(&earlier_label))
        .and_then(|()| remove_if_present(&layout.data_root().join(NOTE)));
    match retired {
        Ok(()) => Report::new(Status::Done),
        Err(error) => Report::failed(format!("the earlier LaunchAgent could not be removed: {error}")),
    }
}

/// No earlier data: the other folders still follow (the window claim as a
/// fence), and an earlier property list left by a finished hand-over goes
/// once the current one is installed.
fn finish_without_hand_over(layout: &Layout, launchd: &mut dyn Launchd, earlier_label: &str) -> io::Result<Report> {
    let mut report = Report::new(Status::Nothing);
    report.folders = match move_fences(layout) {
        Ok(moved) => moved,
        Err(blocked) => return Ok(blocked),
    };
    report.folders.extend(move_best_effort_folders(layout));
    report.config = Some(config_paths::prepare_config_path(&layout.data_root().join("config.toml"))?);
    let earlier_plist = layout.plist(earlier_label);
    if earlier_plist.is_file()
        && layout.plist(&layout.label).is_file()
        && !launchd.loaded(&layout.service(earlier_label))?
    {
        remove_if_present(&earlier_plist)?;
    }
    Ok(report)
}

/// Puts the earlier service back as it was after a step that moved no data
/// (also when an interrupted run had already stopped it), and returns
/// `report` with any failure to do so added. The note is removed only once
/// the earlier state is back: until then it is the only record that the
/// earlier service should run, for the next attempt or the next launch.
fn restore_earlier(
    layout: &Layout,
    launchd: &mut dyn Launchd,
    earlier_label: &str,
    data: &FolderMove,
    note: Note,
    mut report: Report,
) -> Report {
    match restart_earlier(layout, launchd, earlier_label, data, note) {
        Ok(true) => {
            let _ = remove_if_present(&data.earlier.join(NOTE));
            let _ = remove_if_present(&data.current.join(NOTE));
        }
        // The data has moved: the note stays with it for the next attempt.
        Ok(false) => {}
        Err(error) => {
            report.status = Status::Failed;
            let first = report.detail.take().map(|detail| format!("{detail}; ")).unwrap_or_default();
            report.detail = Some(format!("{first}the earlier Hub service could not be started again: {error}"));
        }
    }
    report
}

/// Whether the earlier state is back: `false` when the data has already
/// moved (nothing to restore), an error when the service would not start.
fn restart_earlier(
    layout: &Layout,
    launchd: &mut dyn Launchd,
    earlier_label: &str,
    data: &FolderMove,
    note: Note,
) -> io::Result<bool> {
    if !is_folder(&data.earlier)? {
        return Ok(false);
    }
    let service = layout.service(earlier_label);
    if note.service_was_loaded && !launchd.loaded(&service)? {
        launchd.bootstrap(&layout.domain, &layout.plist(earlier_label))?;
        if !launchd.loaded(&service)? {
            return Err(io::Error::other("launchd did not load it"));
        }
    }
    Ok(true)
}

/// Moves the folders that hold a lock (the window claim): each must move, or
/// be kept beside the current one with its earlier lock free. A held lock
/// defers, anything else fails; either way the caller starts nothing.
fn move_fences(layout: &Layout) -> Result<Vec<(String, String)>, Report> {
    let mut moved = Vec::new();
    for (name, plan, _) in layout.other_moves().into_iter().filter(|(_, _, fence)| *fence) {
        let Some(plan) = plan else { continue };
        let mut outcome = folders::move_folder(&plan);
        // A current Hub holds its claim under both spellings: a claim held
        // under the current name too is its own, and the earlier folder (which
        // it recreates to hold the earlier lock) stays beside the current one.
        if matches!(outcome, Ok(FolderOutcome::Deferred)) && folders::held_under_current(&plan) {
            outcome = Ok(FolderOutcome::BothKept);
        }
        moved.push((name.to_owned(), describe(&outcome)));
        let blocked = match outcome {
            Ok(FolderOutcome::Deferred) => Report {
                detail: Some(format!(
                    "a Hub under the earlier name still holds the {name} claim; quit it, then try again"
                )),
                ..Report::new(Status::Deferred)
            },
            Ok(FolderOutcome::NotAFolder) => {
                Report::failed(format!("the earlier {name} folder is not a plain folder and was left alone"))
            }
            Err(error) => Report::failed(format!("the {name} folder could not be moved: {error}")),
            Ok(_) => continue,
        };
        return Err(Report { folders: moved, ..blocked });
    }
    Ok(moved)
}

/// Moves the folders without a lock (logs, caches); a failure is reported, never fatal.
fn move_best_effort_folders(layout: &Layout) -> Vec<(String, String)> {
    layout
        .other_moves()
        .into_iter()
        .filter(|(_, _, fence)| !*fence)
        .filter_map(|(name, plan, _)| Some((name.to_owned(), describe(&folders::move_folder(&plan?)))))
        .collect()
}

fn describe(outcome: &io::Result<FolderOutcome>) -> String {
    match outcome {
        Ok(outcome) => serde_json::to_value(outcome)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default(),
        Err(error) => format!("failed: {}", error.kind()),
    }
}

fn wait_unloaded(launchd: &mut dyn Launchd, service: &str) -> io::Result<()> {
    let deadline = std::time::Instant::now() + UNLOAD_DEADLINE;
    loop {
        if !launchd.loaded(service)? {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the earlier Hub service did not stop"));
        }
        std::thread::sleep(UNLOAD_POLL);
    }
}

fn read_note(folder: &Path) -> io::Result<Option<Note>> {
    match fs::read(folder.join(NOTE)) {
        Ok(bytes) if bytes.len() <= 4096 => Ok(serde_json::from_slice(&bytes).ok()),
        Ok(_) => Ok(None),
        Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_note(folder: &Path, note: &Note) -> io::Result<()> {
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};
    let temporary = folder.join(format!(".{NOTE}.tmp"));
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
    file.write_all(&serde_json::to_vec(note).map_err(io::Error::other)?)?;
    file.sync_all()?;
    fs::rename(&temporary, folder.join(NOTE))?;
    fs::File::open(folder)?.sync_all()
}

fn is_folder(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_dir()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn folder_has_content(path: &Path) -> io::Result<bool> {
    if !is_folder(path)? {
        return Ok(false);
    }
    Ok(fs::read_dir(path)?.next().is_some())
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// launchd through `/bin/launchctl` (never in unit tests).
#[cfg(target_os = "macos")]
pub struct SystemLaunchd;

#[cfg(target_os = "macos")]
impl SystemLaunchd {
    fn run(arguments: &[&std::ffi::OsStr]) -> io::Result<std::process::Output> {
        #[cfg(test)]
        {
            let _ = arguments;
            Err(io::Error::new(io::ErrorKind::Unsupported, "real launchctl is unavailable in unit tests"))
        }
        #[cfg(not(test))]
        {
            std::process::Command::new("/bin/launchctl").args(arguments).output()
        }
    }

    fn run_ok(arguments: &[&std::ffi::OsStr]) -> io::Result<()> {
        let output = Self::run(arguments)?;
        if output.status.success() {
            return Ok(());
        }
        Err(io::Error::other(format!(
            "launchctl {} failed ({}): {}",
            arguments.first().and_then(|argument| argument.to_str()).unwrap_or("command"),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[cfg(target_os = "macos")]
impl Launchd for SystemLaunchd {
    fn loaded(&mut self, service: &str) -> io::Result<bool> {
        let output = Self::run(&["print".as_ref(), service.as_ref()])?;
        if output.status.success() {
            return Ok(true);
        }
        if output.status.code() == Some(113) {
            return Ok(false);
        }
        Err(io::Error::other(format!("launchctl print failed ({})", output.status)))
    }

    fn disabled(&mut self, domain: &str, label: &str) -> io::Result<bool> {
        let output = Self::run(&["print-disabled".as_ref(), domain.as_ref()])?;
        if !output.status.success() {
            return Err(io::Error::other(format!("launchctl print-disabled failed ({})", output.status)));
        }
        Ok(print_disabled_lists(&String::from_utf8_lossy(&output.stdout), label))
    }

    fn bootout(&mut self, service: &str) -> io::Result<()> {
        let output = Self::run(&["bootout".as_ref(), service.as_ref()])?;
        // 3 ("No such process"): it stopped by itself in between.
        if output.status.success() || output.status.code() == Some(3) {
            return Ok(());
        }
        Err(io::Error::other(format!("launchctl bootout failed ({})", output.status)))
    }

    fn bootstrap(&mut self, domain: &str, plist: &Path) -> io::Result<()> {
        Self::run_ok(&["bootstrap".as_ref(), domain.as_ref(), plist.as_os_str()])
    }

    fn disable(&mut self, service: &str) -> io::Result<()> {
        Self::run_ok(&["disable".as_ref(), service.as_ref()])
    }
}

/// Whether `launchctl print-disabled` lists `label` as disabled.
pub fn print_disabled_lists(output: &str, label: &str) -> bool {
    let quoted = format!("\"{label}\"");
    output.lines().any(|line| {
        let line = line.trim();
        line.starts_with(&quoted)
            && matches!(line[quoted.len()..].trim().strip_prefix("=>").map(str::trim), Some("disabled" | "true"))
    })
}
