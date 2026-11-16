// SPDX-License-Identifier: AGPL-3.0-only

//! Moves a folder from its earlier name to its current one (DEC-160).
//!
//! One `rename` with no-replace semantics on the same volume: the folder is
//! either wholly at the earlier path or wholly at the current one, so a crash
//! at any point leaves a state the next run finishes. A current folder that
//! already holds anything is never overwritten: the current one wins and the
//! earlier one is left untouched (reported as both kept). An empty current
//! folder (one some step created before the move) is removed first.
//!
//! A process may still be using the earlier folder: each lock file it holds
//! there (the Hub lifetime lock, the host claim) is tried without blocking and
//! kept locked across the rename, so a busy folder is deferred, never moved
//! from under its owner. The locks are tried first, so a held one defers the
//! step also when both folders exist.

use std::{
    io,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
};

use rustix::{
    fs::{CWD, FlockOperation, Mode, OFlags, RenameFlags, flock, open, renameat_with},
    io::Errno,
};

/// What happened to one folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderOutcome {
    /// The current and earlier names are the same (before the rename).
    Unchanged,
    /// Neither folder exists.
    Absent,
    /// Only the current folder exists.
    AlreadyMoved,
    /// The earlier folder now has the current name.
    Moved,
    /// Both hold something: the current one is used, the earlier one is left.
    BothKept,
    /// A process still holds a lock in the earlier folder; try again later.
    Deferred,
    /// The earlier path is not a plain folder (a link or a file): left alone.
    NotAFolder,
}

/// A folder to move and the lock files (relative to it) whose holders must be gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderMove {
    pub earlier: PathBuf,
    pub current: PathBuf,
    pub locks: Vec<PathBuf>,
}

impl FolderMove {
    /// The move for `current` whose last component carries the current
    /// spelling; `None` before the rename.
    pub fn for_current(current: PathBuf, locks: &[&str]) -> Option<Self> {
        let name = current.file_name()?.to_str()?;
        let earlier = current.with_file_name(super::spelling::earlier(name)?);
        Some(Self { earlier, current, locks: locks.iter().map(PathBuf::from).collect() })
    }
}

/// Moves `earlier` to `current` (see the module notes). Idempotent.
pub fn move_folder(plan: &FolderMove) -> io::Result<FolderOutcome> {
    if plan.earlier == plan.current {
        return Ok(FolderOutcome::Unchanged);
    }
    let earlier = match std::fs::symlink_metadata(&plan.earlier) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(if exists(&plan.current)? { FolderOutcome::AlreadyMoved } else { FolderOutcome::Absent });
        }
        Err(error) => return Err(error),
    };
    if !earlier.file_type().is_dir() {
        return Ok(FolderOutcome::NotAFolder);
    }
    // A held lock defers the step even when both folders exist: its holder
    // still works under the earlier name (a window claim is a host-wide fence).
    let Some(_held) = hold_locks(&plan.earlier, &plan.locks)? else {
        return Ok(FolderOutcome::Deferred);
    };
    if exists(&plan.current)? && !remove_if_empty_folder(&plan.current)? {
        return Ok(FolderOutcome::BothKept);
    }
    match renameat_with(CWD, &plan.earlier, CWD, &plan.current, RenameFlags::NOREPLACE) {
        Ok(()) => {}
        Err(Errno::EXIST | Errno::NOTEMPTY) => return Ok(FolderOutcome::BothKept),
        Err(error) => return Err(error.into()),
    }
    sync_parent(&plan.current)?;
    Ok(FolderOutcome::Moved)
}

fn exists(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Removes `path` when it is an empty folder; `rmdir` refuses anything else.
fn remove_if_empty_folder(path: &Path) -> io::Result<bool> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Ok(false);
    }
    match std::fs::remove_dir(path) {
        Ok(()) => Ok(true),
        Err(error) if error.raw_os_error() == Some(Errno::NOTEMPTY.raw_os_error()) => Ok(false),
        Err(error) if error.raw_os_error() == Some(Errno::EXIST.raw_os_error()) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Takes every existing lock without waiting; `None` when one is held elsewhere.
fn hold_locks(folder: &Path, locks: &[PathBuf]) -> io::Result<Option<Vec<OwnedFd>>> {
    let mut held = Vec::new();
    for lock in locks {
        let path = folder.join(lock);
        let fd = match open(&path, OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => continue,
            Err(error) => return Err(error.into()),
        };
        match flock(&fd, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => held.push(fd),
            Err(Errno::WOULDBLOCK) => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(held))
}

/// Whether every lock of `plan` is held under its current folder too. A Hub
/// of the current name takes its host claim under both spellings, the earlier
/// one first (`HostClaim`), so a claim held under both names belongs to a
/// current Hub, which also guards the earlier name: no earlier Hub can be
/// collecting. An error or a free lock answers `false`.
pub fn held_under_current(plan: &FolderMove) -> bool {
    !plan.locks.is_empty()
        && plan.locks.iter().all(|lock| {
            open(&plan.current.join(lock), OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
                .is_ok_and(|fd| matches!(flock(&fd, FlockOperation::NonBlockingLockExclusive), Err(Errno::WOULDBLOCK)))
        })
}

fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn plan(root: &Path) -> FolderMove {
        FolderMove {
            earlier: root.join("Earlier Hub"),
            current: root.join("Current Hub"),
            locks: vec![PathBuf::from("data/.hub-instance.lock")],
        }
    }

    fn seed_earlier(plan: &FolderMove) {
        fs::create_dir_all(plan.earlier.join("data")).unwrap();
        fs::write(plan.earlier.join("config.toml"), "bind = \"127.0.0.1:1\"\n").unwrap();
        fs::write(plan.earlier.join("data/hub.sqlite"), "catalogue").unwrap();
        fs::write(plan.earlier.join("data/.hub-instance.lock"), "").unwrap();
    }

    #[test]
    fn the_earlier_folder_moves_whole_and_a_second_run_does_nothing() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(root.path());
        seed_earlier(&plan);
        assert_eq!(move_folder(&plan).unwrap(), FolderOutcome::Moved);
        assert!(!plan.earlier.exists());
        assert_eq!(fs::read_to_string(plan.current.join("data/hub.sqlite")).unwrap(), "catalogue");
        assert_eq!(move_folder(&plan).unwrap(), FolderOutcome::AlreadyMoved);
        assert_eq!(fs::read_to_string(plan.current.join("data/hub.sqlite")).unwrap(), "catalogue");
    }

    #[test]
    fn nothing_to_move_and_unchanged_names_are_no_ops() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(root.path());
        assert_eq!(move_folder(&plan).unwrap(), FolderOutcome::Absent);
        let same = FolderMove { earlier: plan.current.clone(), current: plan.current.clone(), locks: vec![] };
        assert_eq!(move_folder(&same).unwrap(), FolderOutcome::Unchanged);
        assert!(!plan.current.exists());
    }

    #[test]
    fn when_both_exist_neither_is_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(root.path());
        seed_earlier(&plan);
        fs::create_dir_all(&plan.current).unwrap();
        fs::write(plan.current.join("config.toml"), "current").unwrap();
        assert_eq!(move_folder(&plan).unwrap(), FolderOutcome::BothKept);
        assert_eq!(fs::read_to_string(plan.current.join("config.toml")).unwrap(), "current");
        assert_eq!(fs::read_to_string(plan.earlier.join("data/hub.sqlite")).unwrap(), "catalogue");
        assert_eq!(move_folder(&plan).unwrap(), FolderOutcome::BothKept);
    }

    #[test]
    fn an_empty_current_folder_left_by_an_interrupted_run_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(root.path());
        seed_earlier(&plan);
        fs::create_dir_all(&plan.current).unwrap();
        assert_eq!(move_folder(&plan).unwrap(), FolderOutcome::Moved);
        assert_eq!(fs::read_to_string(plan.current.join("data/hub.sqlite")).unwrap(), "catalogue");
    }

    #[test]
    fn a_folder_whose_lock_is_held_is_deferred_and_moves_once_released() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(root.path());
        seed_earlier(&plan);
        let holder = fs::File::open(plan.earlier.join("data/.hub-instance.lock")).unwrap();
        flock(&holder, FlockOperation::NonBlockingLockExclusive).unwrap();
        assert_eq!(move_folder(&plan).unwrap(), FolderOutcome::Deferred);
        assert!(plan.earlier.join("data/hub.sqlite").exists());
        assert!(!plan.current.exists());
        drop(holder);
        assert_eq!(move_folder(&plan).unwrap(), FolderOutcome::Moved);
    }

    #[test]
    fn a_held_lock_defers_even_when_both_folders_exist() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(root.path());
        seed_earlier(&plan);
        fs::create_dir_all(&plan.current).unwrap();
        fs::write(plan.current.join("config.toml"), "current").unwrap();
        let holder = fs::File::open(plan.earlier.join("data/.hub-instance.lock")).unwrap();
        flock(&holder, FlockOperation::NonBlockingLockExclusive).unwrap();
        assert_eq!(move_folder(&plan).unwrap(), FolderOutcome::Deferred);
        drop(holder);
        assert_eq!(move_folder(&plan).unwrap(), FolderOutcome::BothKept);
    }

    #[test]
    fn an_earlier_link_or_file_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(root.path());
        fs::create_dir_all(root.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(root.path().join("elsewhere"), &plan.earlier).unwrap();
        assert_eq!(move_folder(&plan).unwrap(), FolderOutcome::NotAFolder);
        assert!(!plan.current.exists());
    }

    #[test]
    fn the_plan_derives_the_earlier_name_only_after_the_rename() {
        let current = PathBuf::from("/Users/owner/Library/Application Support/Tesatlas Hub");
        let derived = FolderMove::for_current(current.clone(), &["x"]).unwrap();
        assert_eq!(derived.earlier, PathBuf::from("/Users/owner/Library/Application Support/Teslatlas Hub")); // rename:keep
        let before = PathBuf::from("/Users/owner/Library/Application Support/Teslatlas Hub"); // rename:keep
        assert_eq!(FolderMove::for_current(before, &[]), None);
    }
}
