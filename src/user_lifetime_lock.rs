// SPDX-License-Identifier: AGPL-3.0-only

//! One local process lock for the Hub data directory.

use std::{
    fs,
    os::{
        fd::{AsFd, OwnedFd},
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use rustix::{
    fs::{FileType, FlockOperation, Mode, OFlags, flock, fstat, open, openat},
    io::Errno,
    process::getuid,
};
use thiserror::Error;

pub const LOCK_FILE_NAME: &str = ".hub-instance.lock";
const LOCK_FILE_MODE: u32 = 0o600;
const DATA_DIRECTORY_MODE: u32 = 0o700;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UserLifetimeLockError {
    #[error("Hub data directory is unsafe")]
    UnsafeInstallationPath,
    #[error("Hub lifetime lock is already held")]
    AlreadyRunning,
    #[error("Hub lifetime lock identity changed while the process was running")]
    LockIdentityChanged,
    #[error("Hub data directory identity changed while the process was running")]
    StoreIdentityChanged,
    #[error("Hub lifetime lock filesystem operation failed")]
    Filesystem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NodeIdentity {
    device: u64,
    inode: u64,
}

impl NodeIdentity {
    fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            #[allow(clippy::unnecessary_cast)]
            device: stat.st_dev as u64,
            inode: stat.st_ino,
        }
    }

    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

/// Holds directory and lock descriptors so a replaced path is rejected before
/// network work.
#[derive(Debug)]
pub(crate) struct UserLifetimeLock {
    data_dir: PathBuf,
    data_dir_fd: OwnedFd,
    lock_fd: OwnedFd,
    data_dir_identity: NodeIdentity,
    lock_identity: NodeIdentity,
}

impl UserLifetimeLock {
    pub(crate) fn acquire(data_dir: &Path) -> Result<Self, UserLifetimeLockError> {
        let data_dir_was_absent = match fs::symlink_metadata(data_dir) {
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => return Err(UserLifetimeLockError::Filesystem),
        };
        fs::create_dir_all(data_dir).map_err(|_| UserLifetimeLockError::Filesystem)?;
        if data_dir_was_absent {
            fs::set_permissions(data_dir, fs::Permissions::from_mode(DATA_DIRECTORY_MODE))
                .map_err(|_| UserLifetimeLockError::Filesystem)?;
        }
        let (data_dir, data_dir_identity) = checked_data_dir(data_dir)?;
        let data_dir_fd = open_data_dir(&data_dir)?;
        if NodeIdentity::from_stat(
            &fstat(&data_dir_fd).map_err(|_| UserLifetimeLockError::Filesystem)?,
        ) != data_dir_identity
        {
            return Err(UserLifetimeLockError::StoreIdentityChanged);
        }

        let lock_fd = open_lock_file(&data_dir_fd)?;
        let lock_identity = NodeIdentity::from_stat(&validate_lock_file(&lock_fd)?);
        match flock(&lock_fd, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(UserLifetimeLockError::AlreadyRunning);
            }
            Err(_) => return Err(UserLifetimeLockError::Filesystem),
        }

        let lock = Self {
            data_dir,
            data_dir_fd,
            lock_fd,
            data_dir_identity,
            lock_identity,
        };
        lock.revalidate()?;
        Ok(lock)
    }

    pub(crate) fn revalidate(&self) -> Result<(), UserLifetimeLockError> {
        let (_, path_identity) = checked_data_dir(&self.data_dir)?;
        if path_identity != self.data_dir_identity {
            return Err(UserLifetimeLockError::StoreIdentityChanged);
        }
        let held_dir = fstat(&self.data_dir_fd).map_err(|_| UserLifetimeLockError::Filesystem)?;
        if NodeIdentity::from_stat(&held_dir) != self.data_dir_identity
            || !safe_data_directory_stat(&held_dir)
        {
            return Err(UserLifetimeLockError::StoreIdentityChanged);
        }
        let path_dir = open_data_dir(&self.data_dir)?;
        if NodeIdentity::from_stat(
            &fstat(&path_dir).map_err(|_| UserLifetimeLockError::Filesystem)?,
        ) != self.data_dir_identity
        {
            return Err(UserLifetimeLockError::StoreIdentityChanged);
        }

        if NodeIdentity::from_stat(&validate_lock_file(&self.lock_fd)?) != self.lock_identity {
            return Err(UserLifetimeLockError::LockIdentityChanged);
        }
        let path_lock = openat(
            &path_dir,
            LOCK_FILE_NAME,
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| UserLifetimeLockError::LockIdentityChanged)?;
        if NodeIdentity::from_stat(
            &validate_lock_file(&path_lock)
                .map_err(|_| UserLifetimeLockError::LockIdentityChanged)?,
        ) != self.lock_identity
        {
            return Err(UserLifetimeLockError::LockIdentityChanged);
        }
        Ok(())
    }

    pub(crate) fn require_store_path(&self, store: &Path) -> Result<(), UserLifetimeLockError> {
        let canonical = fs::canonicalize(store).map_err(|_| UserLifetimeLockError::Filesystem)?;
        if canonical != self.data_dir {
            return Err(UserLifetimeLockError::UnsafeInstallationPath);
        }
        self.revalidate()
    }

    #[cfg(test)]
    pub(crate) fn lock_path(&self) -> PathBuf {
        self.data_dir.join(LOCK_FILE_NAME)
    }
}

fn checked_data_dir(path: &Path) -> Result<(PathBuf, NodeIdentity), UserLifetimeLockError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| UserLifetimeLockError::Filesystem)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != getuid().as_raw()
        || metadata.permissions().mode() & 0o777 != DATA_DIRECTORY_MODE
    {
        return Err(UserLifetimeLockError::UnsafeInstallationPath);
    }
    let canonical = fs::canonicalize(path).map_err(|_| UserLifetimeLockError::Filesystem)?;
    Ok((canonical, NodeIdentity::from_metadata(&metadata)))
}

fn safe_data_directory_stat(stat: &rustix::fs::Stat) -> bool {
    #[allow(clippy::unnecessary_cast)]
    let mode = stat.st_mode as u32;
    FileType::from_raw_mode(stat.st_mode).is_dir()
        && stat.st_uid == getuid().as_raw()
        && (mode & 0o777) == DATA_DIRECTORY_MODE
}

fn open_data_dir(path: &Path) -> Result<OwnedFd, UserLifetimeLockError> {
    open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| UserLifetimeLockError::Filesystem)
}

fn open_lock_file(data_dir_fd: &impl AsFd) -> Result<OwnedFd, UserLifetimeLockError> {
    for _ in 0..32 {
        match openat(
            data_dir_fd,
            LOCK_FILE_NAME,
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => return Ok(fd),
            Err(Errno::NOENT) => match openat(
                data_dir_fd,
                LOCK_FILE_NAME,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(LOCK_FILE_MODE as _),
            ) {
                Ok(fd) => return Ok(fd),
                Err(Errno::EXIST) => continue,
                Err(_) => return Err(UserLifetimeLockError::Filesystem),
            },
            Err(_) => return Err(UserLifetimeLockError::Filesystem),
        }
    }
    Err(UserLifetimeLockError::Filesystem)
}

fn validate_lock_file(fd: &impl AsFd) -> Result<rustix::fs::Stat, UserLifetimeLockError> {
    let stat = fstat(fd).map_err(|_| UserLifetimeLockError::Filesystem)?;
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || (stat.st_mode as u32 & 0o777) != LOCK_FILE_MODE
    {
        return Err(UserLifetimeLockError::UnsafeInstallationPath);
    }
    Ok(stat)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    #[test]
    fn duplicate_process_is_rejected_then_drop_allows_restart() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let data_dir = temporary.path().join("data");
        let first = UserLifetimeLock::acquire(&data_dir).expect("first lock");
        assert_eq!(
            UserLifetimeLock::acquire(&data_dir).expect_err("second lock"),
            UserLifetimeLockError::AlreadyRunning
        );
        drop(first);
        UserLifetimeLock::acquire(&data_dir).expect("restart lock");
    }

    #[test]
    fn rejects_bad_lock_file_or_data_dir_symlink() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let lock = temporary.path().join(LOCK_FILE_NAME);
        fs::write(&lock, b"").expect("lock file");
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).expect("bad mode");
        assert!(UserLifetimeLock::acquire(temporary.path()).is_err());

        let outside = crate::private_tempdir().expect("outside directory");
        let linked = temporary.path().join("linked");
        symlink(outside.path(), &linked).expect("data directory symlink");
        assert!(UserLifetimeLock::acquire(&linked).is_err());

        let lock_link_dir = crate::private_tempdir().expect("lock link directory");
        let outside_lock = outside.path().join("outside-lock");
        fs::write(&outside_lock, b"").expect("outside lock");
        symlink(&outside_lock, lock_link_dir.path().join(LOCK_FILE_NAME)).expect("lock symlink");
        assert!(UserLifetimeLock::acquire(lock_link_dir.path()).is_err());
    }

    #[test]
    fn creates_private_data_directory_and_rejects_weakened_mode() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let data_dir = temporary.path().join("data");
        let lock = UserLifetimeLock::acquire(&data_dir).expect("private data directory");
        assert_eq!(
            fs::metadata(&data_dir)
                .expect("data metadata")
                .permissions()
                .mode()
                & 0o777,
            DATA_DIRECTORY_MODE
        );
        drop(lock);

        fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o755))
            .expect("weaken data mode");
        assert_eq!(
            UserLifetimeLock::acquire(&data_dir).expect_err("weakened mode rejected"),
            UserLifetimeLockError::UnsafeInstallationPath
        );
    }

    #[test]
    fn revalidation_detects_lock_path_replacement() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let lock = UserLifetimeLock::acquire(temporary.path()).expect("lock");
        let lock_path = lock.lock_path();
        fs::remove_file(&lock_path).expect("remove lock path");
        fs::write(&lock_path, b"").expect("replace lock path");
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).expect("lock mode");
        assert_eq!(
            lock.revalidate().expect_err("replacement rejected"),
            UserLifetimeLockError::LockIdentityChanged
        );
    }
}
