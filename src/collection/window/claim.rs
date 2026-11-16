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
///
/// After the rename (DEC-160) the claim is held under both spellings for its
/// whole life: the earlier spelling's lock first, then the current one. A Hub
/// of the earlier name (an old binary started by hand) claims only the earlier
/// path, so it is refused while this claim lives, and this claim is refused
/// while it collects. Because the current lock is taken only after the earlier
/// one, a held current lock shows that its holder also holds the earlier one
/// (see `name_migration::folders::held_under_current`).
#[derive(Debug)]
pub struct HostClaim {
    _locks: Vec<FileLock>,
}

impl HostClaim {
    pub fn acquire(path: &Path) -> Result<Self, Refusal> {
        let mut locks = Vec::new();
        for path in Self::paths(path) {
            match FileLock::acquire(&path, LockMode::Exclusive, false)? {
                Some(lock) => {
                    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
                    locks.push(lock);
                }
                None => return Err(Refusal::CollectorBusy),
            }
        }
        Ok(Self { _locks: locks })
    }

    /// The lock files a claim at `path` holds, earlier spelling first; only
    /// `path` before the rename.
    pub(crate) fn paths(path: &Path) -> Vec<PathBuf> {
        let earlier = crate::platform::name_migration::spelling::renamed()
            .then(|| {
                let folder = path.parent()?;
                let name = crate::platform::name_migration::spelling::earlier(folder.file_name()?.to_str()?)?;
                Some(folder.with_file_name(name).join(path.file_name()?))
            })
            .flatten();
        earlier.into_iter().chain([path.to_path_buf()]).collect()
    }

    /// The fixed per-user path, outside the Hub data directory.
    pub fn default_path(home: &Path) -> PathBuf {
        home.join("Library/Application Support/Tesatlas Hub Window/host-claim.lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::name_migration::spelling;

    const CURRENT: &str = "Tesatlas Hub Window";
    const EARLIER: &str = "Teslatlas Hub Window"; // rename:keep
    const LOCK: &str = "host-claim.lock";

    /// REV2-CX-M08-T12A finding 4: after the rename the claim holds the
    /// earlier spelling too, for its whole life, so an old binary started by
    /// hand cannot collect beside the current Hub, nor the other way round.
    #[test]
    fn after_the_rename_the_claim_holds_both_spellings_for_its_life() {
        let _renamed = spelling::assume_renamed();
        let home = tempfile::tempdir().unwrap();
        let current = home.path().join(CURRENT).join(LOCK);
        let earlier = home.path().join(EARLIER).join(LOCK);
        assert_eq!(HostClaim::paths(&current), vec![earlier.clone(), current.clone()]);

        let claim = HostClaim::acquire(&current).unwrap();
        // The earlier folder was recreated to hold the lock; an earlier Hub,
        // which claims only its own path, is refused for as long as the claim lives.
        assert!(FileLock::acquire(&earlier, LockMode::Exclusive, false).unwrap().is_none());
        assert!(FileLock::acquire(&current, LockMode::Exclusive, false).unwrap().is_none());
        drop(claim);

        // An earlier Hub collecting: the current claim is refused and leaves no lock behind.
        let old = FileLock::acquire(&earlier, LockMode::Exclusive, false).unwrap().unwrap();
        assert!(matches!(HostClaim::acquire(&current), Err(Refusal::CollectorBusy)));
        assert!(FileLock::acquire(&current, LockMode::Exclusive, false).unwrap().is_some());
        drop(old);
        assert!(HostClaim::acquire(&current).is_ok());
    }

    #[test]
    fn before_the_rename_the_claim_holds_one_path() {
        let path = PathBuf::from("/Users/owner").join(CURRENT).join(LOCK);
        if !spelling::renamed() {
            assert_eq!(HostClaim::paths(&path), vec![path.clone()]);
        }
        let unrenamed = PathBuf::from("/Users/owner/Example Window").join(LOCK);
        let _renamed = spelling::assume_renamed();
        assert_eq!(HostClaim::paths(&unrenamed), vec![unrenamed.clone()]);
    }
}
