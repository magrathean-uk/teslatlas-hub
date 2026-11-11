// SPDX-License-Identifier: AGPL-3.0-only

//! File locks: the per-host collector claim and the window lock.

use std::{
    fs,
    os::{fd::OwnedFd, unix::fs::PermissionsExt},
    path::{Path, PathBuf},
};

use rustix::{
    fs::{FlockOperation, Mode, OFlags, flock, open},
    io::Errno,
};

use super::{ledger, refusal::Refusal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    Shared,
    Exclusive,
}

/// An advisory `flock` held for the life of the value. Locks belong to the
/// open file description, so a second acquire in the same process conflicts
/// like one from another process.
#[derive(Debug)]
pub struct FileLock {
    _fd: OwnedFd,
}

impl FileLock {
    /// Locks `path` (created 0600 with a 0700 parent). `blocking` waits for
    /// the lock; otherwise `Ok(None)` means it is held elsewhere.
    pub fn acquire(path: &Path, mode: LockMode, blocking: bool) -> Result<Option<Self>, Refusal> {
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            ledger::create_private_dir(parent).map_err(|_| Refusal::Io)?;
        }
        let fd = open(
            path,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| Refusal::Io)?;
        let operation = match (mode, blocking) {
            (LockMode::Shared, true) => FlockOperation::LockShared,
            (LockMode::Shared, false) => FlockOperation::NonBlockingLockShared,
            (LockMode::Exclusive, true) => FlockOperation::LockExclusive,
            (LockMode::Exclusive, false) => FlockOperation::NonBlockingLockExclusive,
        };
        match flock(&fd, operation) {
            Ok(()) => Ok(Some(Self { _fd: fd })),
            Err(Errno::WOULDBLOCK) => Ok(None),
            Err(_) => Err(Refusal::Io),
        }
    }
}

/// The exclusive per-host claim on collection, held from the moment a window
/// is opened until it is Closed. A second Hub on the same host is refused.
#[derive(Debug)]
pub struct HostClaim {
    _lock: FileLock,
}

impl HostClaim {
    pub fn acquire(path: &Path) -> Result<Self, Refusal> {
        match FileLock::acquire(path, LockMode::Exclusive, false)? {
            Some(lock) => {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
                Ok(Self { _lock: lock })
            }
            None => Err(Refusal::CollectorBusy),
        }
    }

    /// The fixed per-user path, outside the Hub data directory.
    pub fn default_path(home: &Path) -> PathBuf {
        home.join("Library/Application Support/Teslatlas Hub Window/host-claim.lock")
    }
}
