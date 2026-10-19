// SPDX-License-Identifier: AGPL-3.0-only

//! Test only (B2 fix round, P3b fix 4, R-P3bF4-7): the file mutations the Hub attempts
//! on the paths its run guard protects, counted at the helper every such write goes
//! through, independently of any guard consultation and whether or not the write
//! changes the final state.
//!
//! An attempt is recorded under the path it was aimed at, and only by the helpers that
//! call [`note`]. Counted (B2 fix round, P3b fix 6, N3):
//!
//! - the terrain cache root (`PrivateDirectory`): creating, writing (through the cache's
//!   `write_counted`), syncing (file and directory), renaming and removing an entry, and
//!   creating (`mkdirat`) or protecting (ACL clear, `fchmod`) a directory it opens;
//! - the pack output (`sync/hub_pack`): the staged files and their SQLite writers, the
//!   staging and content directories it creates, the final and temporary compressed
//!   files, the removal of stale staging files with its directory sync, and the sync of
//!   a pack's parent directory;
//! - the transient inspection files of the lineage catalogue (`LineagePackInspection`:
//!   create, write, sync and the removal when it is dropped);
//! - the credential files (`secrets/`, the cursor key) of the Fleet and TeslaMate units.
//!
//! Not counted: the resident control socket (the unlink, bind and chmod of its path, and
//! the removal when it is dropped), the removal of packs by repair, import and the
//! updates delivery, the publication of the schema-22 pack, the creation and tightening of
//! the data directories when a store is opened, and credential recovery and the data
//! backup commands. On a platform without extended
//! ACLs (everything but macOS) the ACL clear is a counted attempt that does nothing. A test
//! reads the attempts under a directory before and after a boundary; a transient write
//! that creates, fills and removes a file leaves the directory as it was and the counter
//! moved. Every test uses directories of its own, so the process-wide registry does not
//! mix tests.

#[cfg(not(test))]
use std::path::Path;

/// Record one attempt to create, write, sync, rename or remove `path`. A no-op outside
/// test builds, so the call stands unconditionally in front of each file mutation.
#[cfg(not(test))]
#[inline(always)]
pub(crate) fn note(_path: &Path) {}

#[cfg(test)]
pub(crate) use counted::{count_under, entries_under, note};

#[cfg(test)]
mod counted {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::{Mutex, OnceLock},
    };

    fn registry() -> &'static Mutex<HashMap<PathBuf, usize>> {
        static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// The path with its deepest existing ancestor resolved, so that a path under a
    /// symlinked temporary directory is the same key however it was spelled and whether or
    /// not the file exists any more.
    fn resolved(path: &Path) -> PathBuf {
        let mut tail = Vec::new();
        let mut current = path.to_path_buf();
        loop {
            if let Ok(base) = current.canonicalize() {
                return tail
                    .iter()
                    .rev()
                    .fold(base, |joined, part| joined.join(part));
            }
            match (current.file_name().map(ToOwned::to_owned), current.parent()) {
                (Some(name), Some(parent)) => {
                    tail.push(name);
                    current = parent.to_path_buf();
                }
                _ => return path.to_path_buf(),
            }
        }
    }

    /// Record one attempt to create, write, sync, rename or remove `path`.
    pub(crate) fn note(path: &Path) {
        let key = resolved(path);
        *registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key)
            .or_insert(0) += 1;
    }

    /// The attempts recorded at or under `directory`, by path (for an assertion message).
    pub(crate) fn entries_under(directory: &Path) -> Vec<(PathBuf, usize)> {
        let base = resolved(directory);
        let mut entries: Vec<(PathBuf, usize)> = registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(path, _)| path.starts_with(&base))
            .map(|(path, count)| (path.clone(), *count))
            .collect();
        entries.sort();
        entries
    }

    /// The attempts recorded at or under `directory`.
    pub(crate) fn count_under(directory: &Path) -> usize {
        let base = resolved(directory);
        registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(path, _)| path.starts_with(&base))
            .map(|(_, count)| *count)
            .sum()
    }
}
