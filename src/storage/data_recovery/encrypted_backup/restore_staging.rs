// SPDX-License-Identifier: AGPL-3.0-only

//! The staging of an encrypted restore, bound to held directories (M06-T15,
//! REV-CX-M06-T11 findings 2, 4 and 5).
//!
//! - The restore's parent is admitted and held open; every namespace
//!   operation in it goes through that descriptor.
//! - The staging is a locked container `.teslatlas-encrypted-restore-<uuid>
//!   .staging` (mode `0700`) holding `data`, the tree being restored. Members
//!   are created with `openat` on held directory descriptors, and SQLite and
//!   the tree checks reach `data` by its own identity (`/.vol/<device>/
//!   <inode>` on macOS, `/proc/self/fd/<n>` on Linux), through no directory
//!   name, so renaming the container, its parent or an ancestor cannot
//!   redirect a write or a check.
//! - Publication renames `data` out of the held container into the held
//!   parent, never replacing anything, after checking that the parent's path
//!   still leads to the held parent; the result must be the held tree.
//! - Before a restore, the parent is swept: an entry with a staging name
//!   (this restore's, or the `.teslatlas-data-recovery-<uuid>.staging` that
//!   directory-format restores, plaintext backups and encrypted restores
//!   before this change used) that is a real directory of this user and that
//!   no process holds locked is claimed by an atomic rename to a fresh sweep
//!   name, re-identified as the object inspected, and emptied through its
//!   descriptor (never a path, never a link). A locked one belongs to a
//!   running restore or backup and stays. A listing, inspection or removal
//!   failure is an error, so no restore extracts plaintext beside abandoned
//!   plaintext it could not remove.

use std::{
    fs::File,
    io,
    os::fd::{AsFd, OwnedFd},
    path::{Path, PathBuf},
};

use rustix::{
    fs::{
        AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, RenameFlags, fchmod, flock, fstat,
        fsync, mkdirat, openat, renameat_with, statat, unlinkat,
    },
    io::Errno,
    process::getuid,
};
use uuid::Uuid;

use super::{
    BackupMember, DataRecoveryError, PRIVATE_DIRECTORY_MODE, PRIVATE_FILE_MODE, StagedTree,
    backup_admission::AdmittedDirectory, hook, invalid, io_error,
};

/// The container of an encrypted restore.
pub(super) const RESTORE_STAGING_PREFIX: &str = ".teslatlas-encrypted-restore-";
pub(super) const RESTORE_STAGING_SUFFIX: &str = ".staging";
/// The staging of directory-format restores and plaintext backups, which
/// encrypted restores also used before M06-T15.
const SHARED_STAGING_PREFIX: &str = ".teslatlas-data-recovery-";
/// A staging directory a sweep has claimed while it empties it.
const SWEEP_PREFIX: &str = ".teslatlas-restore-sweep-";
const STAGED_DATA: &str = "data";
/// The deepest tree a sweep or a discarded staging removes.
const MAX_TREE_DEPTH: usize = 8;

type Identity = (u64, u64);

fn uuid_between(name: &str, prefix: &str, suffix: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
        .is_some_and(|id| Uuid::parse_str(id).is_ok_and(|uuid| uuid.hyphenated().to_string() == id))
}

/// A name the sweep considers: an encrypted restore's container, a shared
/// (directory-format or earlier encrypted) staging directory, or a claim
/// left by an interrupted sweep.
pub(super) fn is_sweepable_name(name: &str) -> bool {
    uuid_between(name, RESTORE_STAGING_PREFIX, RESTORE_STAGING_SUFFIX)
        || uuid_between(name, SHARED_STAGING_PREFIX, RESTORE_STAGING_SUFFIX)
        || uuid_between(name, SWEEP_PREFIX, "")
}

fn open_directory_at(directory: &impl AsFd, name: &str) -> Result<OwnedFd, Errno> {
    openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

fn identity_of(fd: &impl AsFd) -> Result<(Identity, u32), Errno> {
    let stat = fstat(fd)?;
    #[allow(clippy::unnecessary_cast)]
    Ok(((stat.st_dev as u64, stat.st_ino as u64), stat.st_uid as u32))
}

/// The identity of the entry `name` in `directory`, without following a link.
fn named_identity(directory: &impl AsFd, name: &str) -> Option<Identity> {
    let stat = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
    #[allow(clippy::unnecessary_cast)]
    Some((stat.st_dev as u64, stat.st_ino as u64))
}

/// The path that reaches the held directory itself, through no directory
/// name; it must lead back to the same object.
fn held_path(fd: &OwnedFd, shown: &Path) -> Result<PathBuf, DataRecoveryError> {
    let unreadable = |error: Errno| io_error("inspecting the restore staging directory", shown, error.into());
    let (identity, _) = identity_of(fd).map_err(unreadable)?;
    #[cfg(target_os = "macos")]
    let path = PathBuf::from(format!("/.vol/{}/{}", identity.0, identity.1));
    #[cfg(target_os = "linux")]
    let path = {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(invalid(
        "this system cannot reach a restore staging directory by its identity",
    ));
    #[allow(unreachable_code)]
    {
        let reached = rustix::fs::stat(&path).map_err(unreadable)?;
        #[allow(clippy::unnecessary_cast)]
        if (reached.st_dev as u64, reached.st_ino as u64) != identity {
            return Err(invalid(
                "the restore staging directory cannot be reached by its identity on this volume",
            ));
        }
        Ok(path)
    }
}

/// Remove everything inside the held directory `directory`, through
/// descriptors only: no path is resolved and no link is followed (a link is
/// removed as an entry). Passes repeat until the directory reads empty.
fn remove_tree_at(directory: &OwnedFd, depth: usize) -> io::Result<()> {
    if depth > MAX_TREE_DEPTH {
        return Err(io::Error::other("the staging tree is deeper than a restore makes"));
    }
    loop {
        let mut removed = false;
        for entry in Dir::read_from(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            let stat = match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(Errno::NOENT) => continue,
                Err(error) => return Err(error.into()),
            };
            let outcome = if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
                let child = openat(
                    directory,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                remove_tree_at(&child, depth + 1)?;
                unlinkat(directory, name, AtFlags::REMOVEDIR)
            } else {
                unlinkat(directory, name, AtFlags::empty())
            };
            match outcome {
                Ok(()) | Err(Errno::NOENT) => removed = true,
                Err(error) => return Err(error.into()),
            }
        }
        if !removed {
            return Ok(());
        }
    }
}

/// The staged tree of one encrypted restore, held by descriptors.
pub(super) struct HeldStaging {
    parent: AdmittedDirectory,
    name: String,
    /// The container; its exclusive lock lives as long as this descriptor.
    container: OwnedFd,
    container_identity: Identity,
    data: Option<(OwnedFd, Identity)>,
    sha256: Option<OwnedFd>,
    packs: Option<OwnedFd>,
    noop: Option<OwnedFd>,
    root: PathBuf,
    done: bool,
}

impl HeldStaging {
    /// Make the locked container and its `data` tree (`packs/sha256`) in the
    /// held parent. When a concurrent sweep takes the lock of a container
    /// just made, that sweep removes it and another name is tried.
    pub(super) fn create(parent: AdmittedDirectory) -> Result<Self, DataRecoveryError> {
        let me = getuid().as_raw();
        for _ in 0..16 {
            let name = format!("{RESTORE_STAGING_PREFIX}{}{RESTORE_STAGING_SUFFIX}", Uuid::new_v4());
            let shown = parent.path.join(&name);
            let failed = |operation: &'static str| {
                let shown = shown.clone();
                move |error: Errno| io_error(operation, &shown, error.into())
            };
            match mkdirat(&parent.fd, name.as_str(), Mode::RWXU) {
                Ok(()) => {}
                Err(Errno::EXIST) => continue,
                Err(error) => return Err(failed("creating staging directory")(error)),
            }
            let container =
                open_directory_at(&parent.fd, &name).map_err(failed("opening staging directory"))?;
            if flock(&container, FlockOperation::NonBlockingLockExclusive).is_err() {
                continue;
            }
            let (container_identity, owner) =
                identity_of(&container).map_err(failed("inspecting staging directory"))?;
            let mut staging = Self {
                parent,
                name,
                container,
                container_identity,
                data: None,
                sha256: None,
                packs: None,
                noop: None,
                root: PathBuf::new(),
                done: false,
            };
            if owner != me
                || named_identity(&staging.parent.fd, &staging.name) != Some(container_identity)
            {
                return Err(invalid("the restore staging directory changed as it was made"));
            }
            staging.make_tree()?;
            return Ok(staging);
        }
        Err(invalid("could not allocate a unique sibling staging directory"))
    }

    fn shown(&self) -> PathBuf {
        self.parent.path.join(&self.name)
    }

    fn make_directory(
        &self,
        directory: &OwnedFd,
        name: &str,
    ) -> Result<OwnedFd, DataRecoveryError> {
        let shown = self.shown().join(name);
        let failed = |error: Errno| io_error("creating restored data directory", &shown, error.into());
        mkdirat(directory, name, Mode::RWXU).map_err(failed)?;
        let made = open_directory_at(directory, name).map_err(failed)?;
        fchmod(&made, Mode::from_raw_mode(PRIVATE_DIRECTORY_MODE as _)).map_err(failed)?;
        Ok(made)
    }

    fn make_tree(&mut self) -> Result<(), DataRecoveryError> {
        let shown = self.shown();
        fchmod(&self.container, Mode::from_raw_mode(PRIVATE_DIRECTORY_MODE as _))
            .map_err(|error| io_error("protecting staging directory", &shown, error.into()))?;
        let data = self.make_directory(&self.container, STAGED_DATA)?;
        let (identity, _) = identity_of(&data)
            .map_err(|error| io_error("inspecting staging directory", &shown, error.into()))?;
        self.root = held_path(&data, &shown)?;
        let packs = self.make_directory(&data, "packs")?;
        self.sha256 = Some(self.make_directory(&packs, "sha256")?);
        self.packs = Some(packs);
        self.data = Some((data, identity));
        Ok(())
    }

    /// Create one member's file, private and new, through the held
    /// directory its path names (the path was checked as canonical).
    pub(super) fn create_member(&mut self, member: &BackupMember) -> Result<File, DataRecoveryError> {
        let relative = member
            .path
            .strip_prefix("data/")
            .ok_or_else(|| invalid("backup member is outside the data payload"))?;
        if relative.starts_with("packs/noop/") && self.noop.is_none() {
            let packs = self.packs.as_ref().ok_or_else(|| invalid("staging is incomplete"))?;
            self.noop = Some(self.make_directory(packs, "noop")?);
        }
        let (directory, name) = if relative == "hub.sqlite" {
            (self.data.as_ref().map(|(fd, _)| fd), relative)
        } else if let Some(name) = relative.strip_prefix("packs/sha256/") {
            (self.sha256.as_ref(), name)
        } else if let Some(name) = relative.strip_prefix("packs/noop/") {
            (self.noop.as_ref(), name)
        } else {
            return Err(invalid(format!("unsafe data-backup member path: {}", member.path)));
        };
        let directory = directory.ok_or_else(|| invalid("staging is incomplete"))?;
        let shown = self.shown().join(relative);
        let file = openat(
            directory,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(PRIVATE_FILE_MODE as _),
        )
        .map_err(|error| io_error("creating restored data member", &shown, error.into()))?;
        fchmod(&file, Mode::from_raw_mode(PRIVATE_FILE_MODE as _))
            .map_err(|error| io_error("protecting restored data member", &shown, error.into()))?;
        Ok(File::from(file))
    }

    /// What a killed process leaves: the tree stays, and the lock goes with
    /// the closed descriptors (tests only).
    #[cfg(test)]
    pub(super) fn leave_as_killed(mut self) {
        self.done = true;
    }

    /// Remove the container's tree through its descriptor, then the empty
    /// container when its name still leads to it.
    fn discard(&mut self) -> io::Result<()> {
        self.data = None;
        self.sha256 = None;
        self.packs = None;
        self.noop = None;
        remove_tree_at(&self.container, 0)?;
        if named_identity(&self.parent.fd, &self.name) == Some(self.container_identity) {
            unlinkat(&self.parent.fd, self.name.as_str(), AtFlags::REMOVEDIR)?;
        }
        Ok(())
    }
}

impl StagedTree for HeldStaging {
    fn root(&self) -> &Path {
        &self.root
    }

    /// Rename the held `data` out of the held container into the held
    /// parent as `destination`, never replacing anything. The parent's path
    /// must still lead to the held parent first, and the result must be the
    /// held tree.
    fn publish_as(mut self, destination: &Path) -> Result<(), DataRecoveryError> {
        let target = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid("destination must name a new directory"))?
            .to_owned();
        let (data_identity, data_sync) = match self.data.as_ref() {
            Some((fd, identity)) => (*identity, fsync(fd)),
            None => return Err(invalid("staging is incomplete")),
        };
        data_sync.map_err(|error| io_error("syncing the restored data directory", &self.root, error.into()))?;
        hook("restore_before_publish", &self.shown())?;
        let parent_identity = identity_of(&self.parent.fd)
            .map_err(|error| io_error("inspecting the restore parent", &self.parent.path, error.into()))?
            .0;
        let parent_named = rustix::fs::stat(&self.parent.path).ok().map(|stat| {
            #[allow(clippy::unnecessary_cast)]
            (stat.st_dev as u64, stat.st_ino as u64)
        });
        if destination.parent() != Some(self.parent.path.as_path())
            || parent_named != Some(parent_identity)
        {
            return Err(invalid(
                "the restore target's folder moved during the restore; nothing was published",
            ));
        }
        renameat_with(
            &self.container,
            STAGED_DATA,
            &self.parent.fd,
            target.as_str(),
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            io_error(
                "atomically publishing data-recovery directory without replacement",
                destination,
                error.into(),
            )
        })?;
        self.done = true;
        if named_identity(&self.parent.fd, &target) != Some(data_identity) {
            return Err(invalid(
                "the published restore is not the staged directory; check the target folder",
            ));
        }
        fsync(&self.parent.fd)
            .map_err(|error| io_error("syncing the restore parent", &self.parent.path, error.into()))?;
        // The container is empty now; it goes when its name still leads to it.
        if named_identity(&self.parent.fd, &self.name) == Some(self.container_identity) {
            let _ = unlinkat(&self.parent.fd, self.name.as_str(), AtFlags::REMOVEDIR);
        }
        Ok(())
    }
}

impl Drop for HeldStaging {
    fn drop(&mut self) {
        if !self.done {
            // A failure leaves an unlocked container that the next restore's
            // sweep removes or reports.
            let _ = self.discard();
        }
    }
}

/// Before a restore: claim and remove what interrupted restores (and
/// interrupted directory-format restores or plaintext backups) left in the
/// held parent. See the module comment for the rule; every failure to list,
/// inspect or remove is returned.
pub(super) fn clear_interrupted_restores(parent: &AdmittedDirectory) -> Result<(), DataRecoveryError> {
    hook("restore_sweep_list", &parent.path)?;
    let listing_failed =
        |error: io::Error| io_error("listing the restore target's folder", &parent.path, error);
    let mut names = Vec::new();
    for entry in Dir::read_from(&parent.fd).map_err(|error| listing_failed(error.into()))? {
        let entry = entry.map_err(|error| listing_failed(error.into()))?;
        if let Ok(name) = entry.file_name().to_str()
            && is_sweepable_name(name)
        {
            names.push(name.to_owned());
        }
    }
    for name in names {
        sweep_one(parent, &name)?;
    }
    Ok(())
}

fn sweep_one(parent: &AdmittedDirectory, name: &str) -> Result<(), DataRecoveryError> {
    let path = parent.path.join(name);
    let me = getuid().as_raw();
    let inspect_failed = |error: Errno| {
        io_error(
            "inspecting an interrupted restore's staging directory",
            &path,
            error.into(),
        )
    };
    hook("restore_sweep_inspect", &path)?;
    let directory = match open_directory_at(&parent.fd, name) {
        Ok(directory) => directory,
        // Gone, or a link or a file: not a staging directory.
        Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => return Ok(()),
        // Another account's directory, which this user cannot read.
        Err(Errno::ACCESS)
            if statat(&parent.fd, name, AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|stat| stat.st_uid != me) =>
        {
            return Ok(());
        }
        Err(error) => return Err(inspect_failed(error)),
    };
    match flock(&directory, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        // A running restore, backup or sweep holds it.
        Err(Errno::WOULDBLOCK) => return Ok(()),
        Err(error) => return Err(inspect_failed(error)),
    }
    let (identity, owner) = identity_of(&directory).map_err(inspect_failed)?;
    if owner != me {
        return Ok(());
    }
    // Claim: one atomic rename to a fresh sweep name, then the entry under
    // that name must be the object inspected and locked.
    let claimed = if name.starts_with(SWEEP_PREFIX) {
        name.to_owned()
    } else {
        hook("restore_sweep_claim", &path)?;
        let claimed = format!("{SWEEP_PREFIX}{}", Uuid::new_v4());
        match renameat_with(&parent.fd, name, &parent.fd, claimed.as_str(), RenameFlags::NOREPLACE) {
            Ok(()) => claimed,
            // Moved away by someone else: only its contents can be reached.
            Err(Errno::NOENT) => String::new(),
            Err(error) => return Err(inspect_failed(error)),
        }
    };
    if !claimed.is_empty() && named_identity(&parent.fd, &claimed) != Some(identity) {
        // Another directory took the name between inspection and claim: it
        // goes back untouched.
        renameat_with(&parent.fd, claimed.as_str(), &parent.fd, name, RenameFlags::NOREPLACE)
            .map_err(|error| {
                io_error(
                    "returning a directory that took an interrupted restore's name",
                    &path,
                    error.into(),
                )
            })?;
    }
    hook("restore_sweep_remove", &path)?;
    // The plaintext goes through the descriptor of the object inspected,
    // wherever its name now is.
    remove_tree_at(&directory, 0).map_err(|error| {
        io_error("removing an interrupted restore's staging directory", &path, error)
    })?;
    if !claimed.is_empty() && named_identity(&parent.fd, &claimed) == Some(identity) {
        unlinkat(&parent.fd, claimed.as_str(), AtFlags::REMOVEDIR).map_err(|error| {
            io_error(
                "removing an interrupted restore's staging directory",
                &path,
                error.into(),
            )
        })?;
    }
    Ok(())
}
