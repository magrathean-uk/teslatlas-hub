// SPDX-License-Identifier: AGPL-3.0-only

//! Private directories a Hub component owns, validated once through a held
//! descriptor and then used only through that descriptor (G2-3f, decisions 23
//! and 36).
//!
//! `open` resolves the longest existing prefix of the requested path once, so
//! system symlinks such as `/var` and `/tmp` are followed, and then walks the
//! resolved components from `/` with `O_NOFOLLOW`, holding each directory
//! while it is checked:
//!
//! - Every existing ancestor is owned by root or by the effective user. A
//!   group- or world-writable ancestor is accepted only when the effective user
//!   owns it, or root owns it with the sticky bit set (the `/tmp` shape).
//! - The leaf must be owned by the effective user and must not be a symlink. A
//!   leaf with a wider mode than `0700` is tightened through its descriptor, so
//!   an install made before this check keeps working.
//! - Missing components are created one at a time with `mkdirat` and tightened
//!   to `0700` with `fchmod` on the descriptor that was opened, never by path,
//!   so nothing is created under an ancestor that failed the rules.
//! - On macOS the mode bits are not the whole access rule: an extended ACL can
//!   grant other users search, add_file or delete_child on a `0700` directory,
//!   and a directory created below one with inheritable entries starts with
//!   them. The leaf, and every component this module creates, has its ACL
//!   cleared through the held descriptor and is then listed again, so a
//!   directory is accepted only when no ACL entry is left (see
//!   [`clear_extended_acl`]). ACLs on ancestors this module did not create are
//!   not inspected, and files already inside an existing leaf keep whatever ACL
//!   they have.
//!
//! The returned [`PrivateDirectory`] holds the leaf descriptor. Every later
//! operation names a single entry relative to that descriptor, so replacing a
//! path component after validation cannot redirect it.

use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::{Component, Path, PathBuf},
};

use rustix::{
    fs::{
        AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, fchmod, flock, fstat, fstatvfs,
        fsync, mkdirat, open, openat, renameat, statat, unlinkat,
    },
    io::Errno,
};
use thiserror::Error;

/// The mode (`Mode::RWXU`) a directory this module creates or tightens gets.
const PRIVATE_MODE: u32 = 0o700;
const PERMISSION_BITS: u32 = 0o7777;

#[cfg(debug_assertions)]
thread_local! {
    static IO_OBSERVER: std::cell::RefCell<Option<Box<dyn FnMut(&str, &Path)>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(debug_assertions)]
pub(crate) fn observe_for_test(observer: Box<dyn FnMut(&str, &Path)>) -> impl Drop {
    struct Reset(Option<Box<dyn FnMut(&str, &Path)>>);
    impl Drop for Reset {
        fn drop(&mut self) { IO_OBSERVER.with(|slot| *slot.borrow_mut() = self.0.take()); }
    }
    Reset(IO_OBSERVER.with(|slot| slot.replace(Some(observer))))
}
#[cfg(debug_assertions)]
fn observe_io(operation: &str, path: &Path) {
    IO_OBSERVER.with(|slot| { if let Some(observer) = slot.borrow_mut().as_mut() { observer(operation, path); } });
}

/// Why a directory is not a trusted private directory. The type is `pub` in a
/// crate-private module so public error enums can carry it as their source.
#[derive(Debug, Error)]
pub enum PrivateDirectoryError {
    #[error("directory path must be absolute and contain only plain components")]
    InvalidPath,
    #[error("directory component is a symbolic link: {0}")]
    Symlink(PathBuf),
    #[error("directory component is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("directory component has an untrusted owner: {0}")]
    UntrustedOwner(PathBuf),
    #[error("directory component is writable by other users: {0}")]
    WritableByOthers(PathBuf),
    #[error("cannot open, create or protect directory {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// What the kernel reports about one directory component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DirectoryFacts {
    pub(crate) uid: u32,
    /// Permission bits including setuid, setgid and sticky.
    pub(crate) mode: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirectoryFault {
    UntrustedOwner,
    WritableByOthers,
}

/// The rule for an existing ancestor of the private directory.
pub(crate) fn ancestor_fault(facts: DirectoryFacts, euid: u32) -> Option<DirectoryFault> {
    if facts.uid != 0 && facts.uid != euid {
        return Some(DirectoryFault::UntrustedOwner);
    }
    let writable_by_others = facts.mode & 0o022 != 0;
    let root_sticky = facts.uid == 0 && facts.mode & 0o1000 != 0;
    if writable_by_others && facts.uid != euid && !root_sticky {
        return Some(DirectoryFault::WritableByOthers);
    }
    None
}

/// A validated private directory, held by descriptor.
#[derive(Debug)]
pub(crate) struct PrivateDirectory {
    descriptor: OwnedFd,
    path: PathBuf,
    /// Test only (B2 fix round, P3b fix 3, R-P3bF3-4): every attempt to create, rename
    /// or remove an entry, counted here at the file boundary whoever asks and whatever
    /// the caller's guard was told, and whether or not the entry changes.
    #[cfg(test)]
    mutation_attempts: std::sync::atomic::AtomicUsize,
}

impl AsFd for PrivateDirectory {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.descriptor.as_fd()
    }
}

/// Validate `path` as a private directory of the effective user and return it
/// held by descriptor. With `create`, missing components are created `0700`.
pub(crate) fn open_validated(
    path: &Path,
    create: bool,
) -> Result<PrivateDirectory, PrivateDirectoryError> {
    open_validated_as(path, create, rustix::process::geteuid().as_raw())
}

fn open_validated_as(
    path: &Path,
    create: bool,
    euid: u32,
) -> Result<PrivateDirectory, PrivateDirectoryError> {
    let components = plain_components(path)?;
    let Some((leaf, parents)) = components.split_last() else {
        return Err(PrivateDirectoryError::InvalidPath);
    };
    let resolved = resolve_existing_prefix(parents)?;
    let root = Path::new("/");
    let base =
        open("/", directory_flags(), Mode::empty()).map_err(|error| io_error(root, error))?;
    check_ancestor(&base, root, euid)?;
    walk(base, root.to_path_buf(), &resolved, leaf, create, euid)
}

/// R73's failed-probe lane retains descriptors without creating, repairing
/// permissions, or touching ACLs. An absent leaf needs no directory custody.
pub(crate) fn open_validated_read_only(path: &Path) -> Result<Option<PrivateDirectory>, PrivateDirectoryError> {
    let components = plain_components(path)?;
    let Some((leaf, parents)) = components.split_last() else { return Err(PrivateDirectoryError::InvalidPath); };
    let resolved = resolve_existing_prefix(parents)?;
    let euid = rustix::process::geteuid().as_raw();
    let root = Path::new("/");
    let mut descriptor = open(root, directory_flags(), Mode::empty()).map_err(|error| io_error(root, error))?;
    check_ancestor(&descriptor, root, euid)?;
    let mut walked = root.to_path_buf();
    for name in &resolved {
        walked.push(name);
        let (child, _) = open_or_create(&descriptor, name, &walked, false)?;
        check_ancestor(&child, &walked, euid)?;
        descriptor = child;
    }
    walked.push(leaf);
    let (descriptor, _) = match open_or_create(&descriptor, leaf, &walked, false) {
        Err(PrivateDirectoryError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    let facts = facts_of(&descriptor, &walked)?;
    if facts.uid != euid { return Err(PrivateDirectoryError::UntrustedOwner(walked)); }
    if facts.mode & 0o022 != 0 { return Err(PrivateDirectoryError::WritableByOthers(walked)); }
    Ok(Some(PrivateDirectory { descriptor, path: walked,
        #[cfg(test)] mutation_attempts: std::sync::atomic::AtomicUsize::new(0) }))
}

/// Walk `parents` and `leaf` below `base`, which the caller has already
/// vetted. Split out so tests can start from a directory they own.
fn walk(
    base: OwnedFd,
    base_path: PathBuf,
    parents: &[OsString],
    leaf: &OsStr,
    create: bool,
    euid: u32,
) -> Result<PrivateDirectory, PrivateDirectoryError> {
    let mut directory = base;
    let mut walked = base_path;
    for name in parents {
        walked.push(name);
        let (child, created) = open_or_create(&directory, name, &walked, create)?;
        let facts = check_ancestor(&child, &walked, euid)?;
        if created && facts.uid == euid {
            protect(&child, &walked)?;
        }
        directory = child;
    }
    walked.push(leaf);
    let (child, created) = open_or_create(&directory, leaf, &walked, create)?;
    let facts = facts_of(&child, &walked)?;
    if facts.uid != euid {
        return Err(PrivateDirectoryError::UntrustedOwner(walked));
    }
    if created || facts.mode & PERMISSION_BITS != PRIVATE_MODE {
        protect(&child, &walked)?;
        let after = facts_of(&child, &walked)?;
        if after.uid != euid || after.mode & PERMISSION_BITS != PRIVATE_MODE {
            return Err(PrivateDirectoryError::WritableByOthers(walked));
        }
    } else {
        // Already `0700`: only an ACL could still let others in. The clearing of one is
        // a change of the directory's metadata, counted in tests even when none is there.
        crate::platform::file_attempts::note(&walked);
        clear_extended_acl(&child, &walked)?;
    }
    Ok(PrivateDirectory {
        descriptor: child,
        path: walked,
        #[cfg(test)]
        mutation_attempts: std::sync::atomic::AtomicUsize::new(0),
    })
}

/// A single entry classified without following a symbolic link (C10 R16).
#[derive(Debug)]
pub(crate) enum EntryState {
    Absent,
    Regular(OwnedFd),
    Unsafe(UnsafeEntry),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnsafeEntry {
    Symlink,
    NotRegular,
}

/// A credential barrier failed; callers must not treat the state as durable.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("credential durability operation {operation} failed (errno {errno})")]
pub(crate) struct DurabilityError {
    pub operation: &'static str,
    pub errno: i32,
}

/// Drain the device queue on macOS, with no weaker fallback (C10 R72).
pub(crate) fn durable_sync_fd(fd: BorrowedFd<'_>) -> Result<(), DurabilityError> {
    #[cfg(test)]
    if SYNC_SEAM.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(seam) = slot.as_mut() else { return false; };
        let regular = rustix::fs::fstat(fd).ok().is_some_and(|facts| FileType::from_raw_mode(facts.st_mode) == FileType::RegularFile);
        seam.calls.push(if regular { "regular" } else { "directory" });
        regular && seam.fail_regular
    }) { return Err(DurabilityError { operation: "test_barrier", errno: 5 }); }
    #[cfg(target_os = "macos")]
    let (operation, result) = ("F_FULLFSYNC", rustix::fs::fcntl_fullfsync(fd));
    #[cfg(not(target_os = "macos"))]
    let (operation, result) = ("fsync", fsync(fd));
    #[cfg(test)] SYNC_OBSERVATIONS.with(|slot| {
        if let Some(events) = slot.borrow_mut().as_mut()
            && let Ok(facts) = rustix::fs::fstat(fd)
        {
            events.push((operation, FileType::from_raw_mode(facts.st_mode) == FileType::RegularFile,
                facts.st_dev as u64, facts.st_ino));
        }
    });
    result.map_err(|error| DurabilityError {
        operation,
        errno: error.raw_os_error(),
    })?;
    #[cfg(debug_assertions)]
    if fstat(fd).is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode).is_file()) {
        observe_io("file_barrier", Path::new(""));
    }
    Ok(())
}

#[cfg(test)]
#[derive(Default)]
struct SyncSeam { fail_regular: bool, calls: Vec<&'static str> }
#[cfg(test)]
thread_local! { static SYNC_SEAM: std::cell::RefCell<Option<SyncSeam>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
thread_local! { static SYNC_OBSERVATIONS: std::cell::RefCell<Option<Vec<(&'static str, bool, u64, u64)>>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
pub(crate) fn with_sync_observations<T>(action: impl FnOnce() -> T) -> (T, Vec<(&'static str, bool, u64, u64)>) {
    struct Reset(Option<Vec<(&'static str, bool, u64, u64)>>);
    impl Drop for Reset { fn drop(&mut self) { SYNC_OBSERVATIONS.with(|slot| *slot.borrow_mut() = self.0.take()); } }
    let reset = Reset(SYNC_OBSERVATIONS.with(|slot| slot.replace(Some(Vec::new()))));
    let value = action();
    let events = SYNC_OBSERVATIONS.with(|slot| slot.borrow_mut().as_mut().unwrap().clone());
    drop(reset); (value, events)
}
#[cfg(test)]
pub(crate) fn with_sync_faults<T>(fail_regular: bool, action: impl FnOnce() -> T) -> (T, Vec<&'static str>) {
    struct Reset(Option<SyncSeam>);
    impl Drop for Reset { fn drop(&mut self) { SYNC_SEAM.with(|slot| *slot.borrow_mut() = self.0.take()); } }
    let reset = Reset(SYNC_SEAM.with(|slot| slot.replace(Some(SyncSeam { fail_regular, calls: Vec::new() }))));
    let value = action(); let calls = SYNC_SEAM.with(|slot| slot.borrow_mut().as_mut().unwrap().calls.clone());
    drop(reset); (value, calls)
}

/// Thread-local syscall controls for CT93; production never consults them.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct EntrySeam {
    pub stat_types: std::collections::VecDeque<FileType>,
    pub stat_error: Option<Errno>,
    pub after_stat: Option<Box<dyn FnOnce()>>,
    pub open_error: Option<Errno>,
    pub opens: usize,
    pub stats: usize,
    pub closed_nonregular: usize,
}

#[cfg(test)]
thread_local! {
    pub(crate) static ENTRY_SEAM: std::cell::RefCell<Option<EntrySeam>> =
        const { std::cell::RefCell::new(None) };
}

impl PrivateDirectory {
    /// The resolved path the directory was validated at. For messages and
    /// tests only: operations use the descriptor, never this path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Open a regular file of this directory for reading without following a
    /// symlink. `None` when the entry is absent, a symlink or not a regular
    /// file.
    pub(crate) fn open_regular(&self, name: &str) -> io::Result<Option<File>> {
        #[cfg(debug_assertions)] observe_io("open", &self.path.join(name));
        single_component(name)?;
        let descriptor = match openat(
            &self.descriptor,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT | Errno::LOOP | Errno::NXIO) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let file = File::from(descriptor);
        Ok(file.metadata()?.is_file().then_some(file))
    }

    /// Create a new `0600` file that must not exist, never through a symlink.
    pub(crate) fn create_exclusive(&self, name: &str) -> io::Result<File> {
        #[cfg(debug_assertions)] observe_io("create", &self.path.join(name));
        #[cfg(test)]
        self.note_file_attempt(name);
        single_component(name)?;
        let descriptor = openat(
            &self.descriptor,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?;
        Ok(File::from(descriptor))
    }

    /// Rename one entry of this directory over another entry of it.
    pub(crate) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        #[cfg(debug_assertions)] { observe_io("rename", &self.path.join(from)); observe_io("rename", &self.path.join(to)); }
        #[cfg(test)]
        {
            self.note_file_attempt(from);
            crate::platform::file_attempts::note(&self.path.join(to));
        }
        single_component(from)?;
        single_component(to)?;
        renameat(&self.descriptor, from, &self.descriptor, to).map_err(io::Error::from)
    }

    /// Remove one entry of this directory (never a directory). Absent is fine.
    pub(crate) fn remove(&self, name: &str) -> io::Result<()> {
        #[cfg(debug_assertions)] observe_io("remove", &self.path.join(name));
        #[cfg(test)]
        self.note_file_attempt(name);
        single_component(name)?;
        match unlinkat(&self.descriptor, name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Test only (R-P3bF4-7): one attempt to create, write, sync, rename or remove the
    /// entry `name` of this directory, counted here and under its path in the registry
    /// of file attempts. Every file mutation of the cache goes through a helper that
    /// calls it, whoever asks and whatever the caller's guard said.
    #[cfg(test)]
    pub(crate) fn note_file_attempt(&self, name: &str) {
        self.mutation_attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        crate::platform::file_attempts::note(&self.path.join(name));
    }

    /// Test only: the creations, content writes, syncs, renames and removals attempted
    /// through this directory (a rename counts once, and is recorded at both names).
    #[cfg(test)]
    pub(crate) fn mutation_attempts(&self) -> usize {
        self.mutation_attempts
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Flush the directory entries to stable storage. A sync leaves the file image as it
    /// was; in tests the attempt is counted all the same (B2 fix round, P3b fix 5,
    /// R-P3bF5-3), against the directory itself.
    pub(crate) fn sync(&self) -> io::Result<()> {
        #[cfg(test)]
        {
            self.mutation_attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::platform::file_attempts::note(&self.path);
        }
        fsync(&self.descriptor).map_err(io::Error::from)
    }

    /// Whether the entry `name` of this directory (not followed if it is a symlink) is
    /// the very file `file` holds open: the same device and inode. A writer or a reclaim
    /// that has locked a file uses it to know that the name still refers to what it holds.
    pub(crate) fn is_entry(&self, name: &str, file: &File) -> io::Result<bool> {
        single_component(name)?;
        let entry = match statat(&self.descriptor, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let held = fstat(file).map_err(io::Error::from)?;
        #[allow(clippy::unnecessary_cast)]
        Ok(entry.st_dev as u64 == held.st_dev as u64 && entry.st_ino == held.st_ino)
    }

    /// Whether the filesystem holding this directory can take an advisory `flock`
    /// (B2 fix round, P3b fix 6, N1): one non-blocking exclusive lock on the held
    /// directory descriptor, let go again at once, so no file or entry is touched. Another
    /// holder is the answer "locking works" (`EWOULDBLOCK`); any other error (`ENOTSUP`,
    /// `ENOLCK`, ...) means the entry locks of the terrain cache cannot be relied on here.
    pub(crate) fn probe_lock_support(&self) -> io::Result<()> {
        #[cfg(test)]
        if let Some(code) = INJECTED_LOCK_ERROR.with(std::cell::Cell::take) {
            return Err(io::Error::from_raw_os_error(code));
        }
        match flock(&self.descriptor, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => flock(&self.descriptor, FlockOperation::Unlock).map_err(io::Error::from),
            Err(Errno::WOULDBLOCK) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// The names of the entries, without `.` and `..`. Names that are not
    /// UTF-8 cannot be ours and are left out.
    pub(crate) fn entry_names(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in Dir::read_from(&self.descriptor)? {
            let entry = entry?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if let Ok(name) = std::str::from_utf8(bytes) {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    /// Bytes available to this user on the filesystem holding the directory.
    pub(crate) fn available_bytes(&self) -> io::Result<u64> {
        let stat = fstatvfs(&self.descriptor)?;
        Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
    }

    /// Classify first, open regular files only, and validate what was opened.
    pub(crate) fn inspect_entry(&self, name: &str) -> Result<EntryState, PrivateDirectoryError> {
        #[cfg(debug_assertions)] observe_io("inspect", &self.path.join(name));
        // The test seam changes the classification, never the production flags.
        fn classify(
            directory: &PrivateDirectory,
            name: &str,
            path: &Path,
        ) -> Result<Option<FileType>, PrivateDirectoryError> {
            #[cfg(test)]
            {
                let injected = ENTRY_SEAM.with(|slot| {
                    let mut slot = slot.borrow_mut();
                    let seam = slot.as_mut()?;
                    seam.stats += 1;
                    if let Some(error) = seam.stat_error.take() {
                        return Some(Err(io_error(path, error)));
                    }
                    seam.stat_types.pop_front().map(|kind| Ok(Some(kind)))
                });
                if let Some(result) = injected {
                    return result;
                }
            }
            match statat(&directory.descriptor, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => Ok(Some(FileType::from_raw_mode(stat.st_mode))),
                Err(Errno::NOENT) => Ok(None),
                Err(error) => Err(io_error(path, error)),
            }
        }

        let path = self.path.join(name);
        single_component(name).map_err(|error| io_error(&path, error))?;
        match classify(self, name, &path)? {
            None => return Ok(EntryState::Absent),
            Some(FileType::Symlink) => return Ok(EntryState::Unsafe(UnsafeEntry::Symlink)),
            Some(kind) if !kind.is_file() => {
                return Ok(EntryState::Unsafe(UnsafeEntry::NotRegular));
            }
            Some(_) => {}
        }

        #[cfg(test)]
        {
            // Run a replacement callback outside the RefCell borrow.
            let after_stat = ENTRY_SEAM.with(|slot| {
                slot.borrow_mut()
                    .as_mut()
                    .and_then(|seam| seam.after_stat.take())
            });
            if let Some(after_stat) = after_stat {
                after_stat();
            }
        }

        #[cfg(test)]
        let injected_error = ENTRY_SEAM.with(|slot| {
            let mut slot = slot.borrow_mut();
            let seam = slot.as_mut()?;
            seam.opens += 1;
            seam.open_error.take()
        });
        #[cfg(not(test))]
        let injected_error: Option<Errno> = None;
        let opened = if let Some(error) = injected_error {
            Err(error)
        } else {
            openat(
                &self.descriptor,
                name,
                OFlags::RDONLY
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::NOCTTY
                    | OFlags::CLOEXEC,
                Mode::empty(),
            )
        };
        match opened {
            Ok(descriptor) => {
                let stat = fstat(&descriptor).map_err(|error| io_error(&path, error))?;
                if FileType::from_raw_mode(stat.st_mode).is_file() {
                    Ok(EntryState::Regular(descriptor))
                } else {
                    drop(descriptor);
                    #[cfg(test)]
                    ENTRY_SEAM.with(|slot| {
                        if let Some(seam) = slot.borrow_mut().as_mut() {
                            seam.closed_nonregular += 1;
                        }
                    });
                    Ok(EntryState::Unsafe(UnsafeEntry::NotRegular))
                }
            }
            Err(Errno::NOENT) => Ok(EntryState::Absent),
            Err(Errno::LOOP) => Ok(EntryState::Unsafe(UnsafeEntry::Symlink)),
            Err(error) => match classify(self, name, &path)? {
                None => Ok(EntryState::Absent),
                Some(FileType::Symlink) => Ok(EntryState::Unsafe(UnsafeEntry::Symlink)),
                Some(kind) if !kind.is_file() => Ok(EntryState::Unsafe(UnsafeEntry::NotRegular)),
                Some(_) => Err(io_error(&path, error)),
            },
        }
    }

    /// Apply the credential durability barrier to the held directory.
    pub(crate) fn durable_sync(&self) -> Result<(), DurabilityError> {
        durable_sync_fd(self.descriptor.as_fd())?;
        #[cfg(debug_assertions)] observe_io("directory_barrier", &self.path);
        #[cfg(test)] crate::auth::credential_vault::fixtures::power_event("directory_barrier", Some(self.path()));
        Ok(())
    }
}

// Test only: the OS error code the next `PrivateDirectory::probe_lock_support` of this
// thread reports instead of asking the filesystem (a root without file locking).
#[cfg(test)]
thread_local! {
    pub(crate) static INJECTED_LOCK_ERROR: std::cell::Cell<Option<i32>> =
        const { std::cell::Cell::new(None) };
}

/// Take an exclusive advisory lock (`flock`) on `file` without waiting: `Ok(true)` when
/// it is held now, `Ok(false)` when another open file description holds it, of this
/// process or of another. The lock belongs to the open file description, so it is kept
/// for as long as `file` or any duplicate of it stays open, and the kernel releases it
/// when the last of them closes, whatever ended the process.
pub(crate) fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    match flock(file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(true),
        Err(Errno::WOULDBLOCK) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Names handed to the operations above are single entries of the directory.
fn single_component(name: &str) -> io::Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private directory entries are single plain names",
        ));
    }
    Ok(())
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn io_error(path: &Path, source: impl Into<io::Error>) -> PrivateDirectoryError {
    PrivateDirectoryError::Io {
        path: path.to_path_buf(),
        source: source.into(),
    }
}

/// The plain components of an absolute path: no `..`, no prefix.
fn plain_components(path: &Path) -> Result<Vec<OsString>, PrivateDirectoryError> {
    if !path.is_absolute() {
        return Err(PrivateDirectoryError::InvalidPath);
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => components.push(name.to_owned()),
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(PrivateDirectoryError::InvalidPath);
            }
        }
    }
    Ok(components)
}

/// Resolve the longest existing prefix of `parents` once, so system symlinks
/// are followed here and nowhere later, and append the components that do not
/// exist yet.
fn resolve_existing_prefix(parents: &[OsString]) -> Result<Vec<OsString>, PrivateDirectoryError> {
    for keep in (0..=parents.len()).rev() {
        let mut candidate = PathBuf::from("/");
        candidate.extend(&parents[..keep]);
        match std::fs::canonicalize(&candidate) {
            Ok(canonical) => {
                let mut resolved = Vec::new();
                for component in canonical.components() {
                    match component {
                        Component::RootDir => {}
                        Component::Normal(name) => resolved.push(name.to_owned()),
                        _ => return Err(PrivateDirectoryError::InvalidPath),
                    }
                }
                resolved.extend_from_slice(&parents[keep..]);
                return Ok(resolved);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(io_error(&candidate, error)),
        }
    }
    Err(PrivateDirectoryError::InvalidPath)
}

/// Open the directory `name` of `parent` without following a symlink.
/// `Ok(None)` when it does not exist.
fn open_child(
    parent: &OwnedFd,
    name: &OsStr,
    path: &Path,
) -> Result<Option<OwnedFd>, PrivateDirectoryError> {
    match openat(parent, name, directory_flags(), Mode::empty()) {
        Ok(descriptor) => Ok(Some(descriptor)),
        Err(Errno::NOENT) => Ok(None),
        // Linux answers a symlink opened with O_NOFOLLOW | O_DIRECTORY with
        // ENOTDIR and macOS with ELOOP: look at the entry to name the fault.
        Err(Errno::LOOP | Errno::NOTDIR) => match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
                Err(PrivateDirectoryError::Symlink(path.to_path_buf()))
            }
            Ok(_) => Err(PrivateDirectoryError::NotDirectory(path.to_path_buf())),
            Err(error) => Err(io_error(path, error)),
        },
        Err(error) => Err(io_error(path, error)),
    }
}

/// Open the child, creating it when allowed. The flag says whether this call
/// created it.
fn open_or_create(
    parent: &OwnedFd,
    name: &OsStr,
    path: &Path,
    create: bool,
) -> Result<(OwnedFd, bool), PrivateDirectoryError> {
    if let Some(child) = open_child(parent, name, path)? {
        return Ok((child, false));
    }
    if !create {
        return Err(io_error(path, io::ErrorKind::NotFound));
    }
    // Counted in tests whatever follows (B2 fix round, P3b fix 5, R-P3bF5-3).
    crate::platform::file_attempts::note(path);
    let created = match mkdirat(parent, name, Mode::RWXU) {
        Ok(()) => true,
        // Someone else created it between the open and the mkdirat: judge it
        // like any existing component.
        Err(Errno::EXIST) => false,
        Err(error) => return Err(io_error(path, error)),
    };
    match open_child(parent, name, path)? {
        Some(child) => Ok((child, created)),
        None => Err(io_error(path, io::ErrorKind::NotFound)),
    }
}

fn facts_of(directory: &OwnedFd, path: &Path) -> Result<DirectoryFacts, PrivateDirectoryError> {
    let stat = fstat(directory).map_err(|error| io_error(path, error))?;
    #[allow(clippy::useless_conversion)]
    let mode = u32::from(stat.st_mode);
    Ok(DirectoryFacts {
        uid: stat.st_uid,
        mode: mode & PERMISSION_BITS,
    })
}

fn check_ancestor(
    directory: &OwnedFd,
    path: &Path,
    euid: u32,
) -> Result<DirectoryFacts, PrivateDirectoryError> {
    let facts = facts_of(directory, path)?;
    match ancestor_fault(facts, euid) {
        None => Ok(facts),
        Some(DirectoryFault::UntrustedOwner) => {
            Err(PrivateDirectoryError::UntrustedOwner(path.to_path_buf()))
        }
        Some(DirectoryFault::WritableByOthers) => {
            Err(PrivateDirectoryError::WritableByOthers(path.to_path_buf()))
        }
    }
}

/// Make a directory the caller has opened and found to be its own private:
/// no extended ACL, and mode `0700`.
fn protect(directory: &OwnedFd, path: &Path) -> Result<(), PrivateDirectoryError> {
    crate::platform::file_attempts::note(path);
    clear_extended_acl(directory, path)?;
    fchmod(directory, Mode::RWXU).map_err(|error| io_error(path, error))
}

/// Remove every extended ACL entry of `directory`, inherited ones included,
/// and check that none is left. A directory that still shows an entry is
/// refused as writable by others.
///
/// macOS has no safe descriptor-based ACL call, and the crate forbids unsafe
/// code, so the removal runs `/bin/chmod -N` on the held descriptor: the
/// descriptor is the child's standard input and the path handed to `chmod` is
/// `/dev/fd/0`, so no path of the cache can be swapped underneath it. A
/// directory without an ACL is a no-op for `chmod -N`. A filesystem that
/// cannot hold ACLs answers "Operation not supported", which means there is
/// nothing to clear.
#[cfg(target_os = "macos")]
pub(crate) fn clear_extended_acl(
    directory: &OwnedFd,
    path: &Path,
) -> Result<(), PrivateDirectoryError> {
    clear_and_verify(directory, path, chmod_clear_acl)
}

/// Run `clear`, then list the ACL of the same directory again instead of
/// trusting the command's exit status.
#[cfg(target_os = "macos")]
fn clear_and_verify(
    directory: &OwnedFd,
    path: &Path,
    clear: impl FnOnce(&OwnedFd, &Path) -> Result<(), PrivateDirectoryError>,
) -> Result<(), PrivateDirectoryError> {
    clear(directory, path)?;
    verify_no_extended_acl(directory, path)
}

#[cfg(target_os = "macos")]
fn chmod_clear_acl(directory: &OwnedFd, path: &Path) -> Result<(), PrivateDirectoryError> {
    // A spawned chmod can transiently refuse its inherited descriptor path.
    // Retry only that exact refusal, with the same directory still held. A
    // successful clear must still pass clear_and_verify's independent listing.
    for attempt in 0..3 {
        let result = chmod_clear_acl_once(directory, path);
        let descriptor_refusal = matches!(&result,
            Err(PrivateDirectoryError::Io { source, .. })
                if source.kind() == io::ErrorKind::Other
                    && source.to_string() == ACL_DESCRIPTOR_REFUSAL);
        if !descriptor_refusal || attempt == 2 { return result; }
        fstat(directory).map_err(|error| io_error(path, error))?;
    }
    unreachable!("bounded ACL attempts return")
}

#[cfg(target_os = "macos")]
const ACL_DESCRIPTOR_REFUSAL: &str = "cannot clear the extended ACL: chmod: /dev/fd/0: No such file or directory";

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    static ACL_DESCRIPTOR_REFUSALS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) fn with_acl_descriptor_refusals<T>(count: usize, action: impl FnOnce() -> T) -> (T, usize) {
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) { ACL_DESCRIPTOR_REFUSALS.with(|slot| slot.set(self.0)); }
    }
    let reset = Reset(ACL_DESCRIPTOR_REFUSALS.with(|slot| slot.replace(count)));
    let result = action();
    let used = ACL_DESCRIPTOR_REFUSALS.with(|slot| count - slot.get());
    drop(reset);
    (result, used)
}

#[cfg(target_os = "macos")]
fn chmod_clear_acl_once(directory: &OwnedFd, path: &Path) -> Result<(), PrivateDirectoryError> {
    use std::process::{Command, Stdio};

    #[cfg(test)]
    if ACL_DESCRIPTOR_REFUSALS.with(|slot| {
        let remaining = slot.get();
        if remaining == 0 { false } else { slot.set(remaining - 1); true }
    }) {
        return Err(io_error(path, io::Error::other(ACL_DESCRIPTOR_REFUSAL)));
    }
    let held = directory
        .try_clone()
        .map_err(|error| io_error(path, error))?;
    let output = Command::new("/bin/chmod")
        .arg("-N")
        .arg("/dev/fd/0")
        .env("LC_ALL", "C")
        .stdin(Stdio::from(held))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| io_error(path, error))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    acl_clear_outcome(output.status.success(), &stderr).map_err(|reason| {
        io_error(
            path,
            io::Error::other(format!("cannot clear the extended ACL: {reason}")),
        )
    })
}

/// List the ACL of the held directory (`ls -e` cannot read one through
/// `/dev/fd`, so it names the directory by `F_GETPATH`, where the kernel says
/// the descriptor's directory is now) and refuse it if any entry is left. The
/// check can only fail closed: a path swapped in the meantime shows another
/// directory's ACL or none, and the descriptor that was cleared stays the one
/// every later operation uses.
#[cfg(target_os = "macos")]
pub(crate) fn verify_no_extended_acl(
    directory: &OwnedFd,
    path: &Path,
) -> Result<(), PrivateDirectoryError> {
    use std::{
        os::unix::ffi::OsStrExt,
        process::{Command, Stdio},
    };

    let current = rustix::fs::getpath(directory).map_err(|error| io_error(path, error))?;
    let output = Command::new("/bin/ls")
        .arg("-lde")
        .arg(OsStr::from_bytes(current.to_bytes()))
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| io_error(path, error))?;
    if !output.status.success() {
        return Err(io_error(
            path,
            io::Error::other("cannot list the directory's ACL"),
        ));
    }
    if acl_entry_count(&String::from_utf8_lossy(&output.stdout)) != 0 {
        return Err(PrivateDirectoryError::WritableByOthers(path.to_path_buf()));
    }
    Ok(())
}

/// The ACL entries in an `ls -lde` listing: the first line is the directory
/// itself, and each entry follows on a line of its own, numbered `0:`, `1:`...
#[cfg(target_os = "macos")]
fn acl_entry_count(listing: &str) -> usize {
    listing
        .lines()
        .skip(1)
        .filter(|line| {
            let line = line.trim_start();
            let digits = line.chars().take_while(char::is_ascii_digit).count();
            digits > 0 && line[digits..].starts_with(':')
        })
        .count()
}

/// Other platforms have no extended ACL on a directory that its mode bits do
/// not show (a POSIX ACL's mask is the group bits, which `0700` clears).
#[cfg(not(target_os = "macos"))]
pub(crate) fn clear_extended_acl(
    _directory: &OwnedFd,
    _path: &Path,
) -> Result<(), PrivateDirectoryError> {
    Ok(())
}

/// What `chmod -N` reported: cleared, or a filesystem with no ACL to clear, or
/// a failure with its first line of explanation.
#[cfg(target_os = "macos")]
fn acl_clear_outcome(success: bool, stderr: &str) -> Result<(), String> {
    if success || stderr.contains("Operation not supported") {
        return Ok(());
    }
    Err(stderr.lines().next().unwrap_or("chmod failed").to_owned())
}

#[cfg(test)]
#[path = "private_directory/tests.rs"]
mod tests;
