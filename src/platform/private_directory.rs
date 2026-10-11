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
        AtFlags, Dir, FileType, Mode, OFlags, fchmod, fstat, fstatvfs, fsync, mkdirat, open,
        openat, renameat, statat, unlinkat,
    },
    io::Errno,
};
use thiserror::Error;

/// The mode (`Mode::RWXU`) a directory this module creates or tightens gets.
const PRIVATE_MODE: u32 = 0o700;
const PERMISSION_BITS: u32 = 0o7777;

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
        // Already `0700`: only an ACL could still let others in.
        clear_extended_acl(&child, &walked)?;
    }
    Ok(PrivateDirectory {
        descriptor: child,
        path: walked,
    })
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
        single_component(from)?;
        single_component(to)?;
        renameat(&self.descriptor, from, &self.descriptor, to).map_err(io::Error::from)
    }

    /// Remove one entry of this directory (never a directory). Absent is fine.
    pub(crate) fn remove(&self, name: &str) -> io::Result<()> {
        single_component(name)?;
        match unlinkat(&self.descriptor, name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Flush the directory entries to stable storage.
    pub(crate) fn sync(&self) -> io::Result<()> {
        fsync(&self.descriptor).map_err(io::Error::from)
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
fn clear_extended_acl(directory: &OwnedFd, path: &Path) -> Result<(), PrivateDirectoryError> {
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
    use std::process::{Command, Stdio};

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
fn verify_no_extended_acl(directory: &OwnedFd, path: &Path) -> Result<(), PrivateDirectoryError> {
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
fn clear_extended_acl(_directory: &OwnedFd, _path: &Path) -> Result<(), PrivateDirectoryError> {
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
