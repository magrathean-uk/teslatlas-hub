// SPDX-License-Identifier: AGPL-3.0-only

//! The directory checks of the plaintext formats: exact member sets, Finder
//! metadata tolerance (M06-T18) and the private modes of restored directories.

use super::*;

pub(super) fn require_directory_entries<const N: usize>(
    path: &Path,
    expected: [&str; N],
    description: &'static str,
) -> Result<(), DataRecoveryError> {
    let expected = expected
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    require_directory_entry_set(path, &expected, description)
}

pub(super) fn require_directory_entry_set(
    path: &Path,
    expected: &BTreeSet<String>,
    description: &'static str,
) -> Result<(), DataRecoveryError> {
    let metadata = require_real_directory(path, description)?;
    if permission_mode(&metadata) != PRIVATE_DIRECTORY_MODE {
        return Err(invalid(format!("{description} must have mode 0700")));
    }
    // Entries are read one at a time: regular Finder metadata is skipped before
    // anything is kept or counted, so only real members meet the entry bound
    // (REV2-CX-M06-T06 finding 2).
    let directory = fs::read_dir(path)
        .map_err(|source| io_error("reading data-backup directory", path, source))?;
    let mut actual = BTreeSet::new();
    for entry in directory {
        let entry = entry
            .map_err(|source| io_error("reading data-backup directory entry", path, source))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid(format!("{description} contains a non-UTF-8 name")))?;
        if !expected.contains(&name) && is_ignorable_finder_file(&entry.path(), &name) {
            continue;
        }
        if !actual.insert(name) {
            return Err(invalid(format!("{description} contains a duplicate name")));
        }
        if actual.len() > LISTED_FORMAT_MAX_MEMBERS + 16 {
            return Err(invalid(
                "data-backup directory entry count exceeds its bound",
            ));
        }
    }
    if &actual != expected {
        return Err(invalid(format!(
            "{description} has unknown or missing members (expected {expected:?}, actual {actual:?})"
        )));
    }
    Ok(())
}

/// macOS Finder metadata (`.DS_Store`, AppleDouble `._*`) that the OS writes
/// into viewed folders; never a backup member, never copied or restored.
fn is_finder_metadata(name: &str) -> bool {
    name == ".DS_Store" || (name.starts_with("._") && name.len() > 2)
}

/// True only for a regular file (not followed through symlinks) with a Finder
/// metadata name; directories, symlinks and other types are never ignored.
pub(super) fn is_ignorable_finder_file(path: &Path, name: &str) -> bool {
    is_finder_metadata(name)
        && fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_file())
            .unwrap_or(false)
}

pub(super) fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, DataRecoveryError> {
    let directory = fs::read_dir(path)
        .map_err(|source| io_error("reading data-backup directory", path, source))?;
    let mut entries = Vec::new();
    for entry in directory {
        entries.push(
            entry
                .map_err(|source| io_error("reading data-backup directory entry", path, source))?,
        );
        if entries.len() > LISTED_FORMAT_MAX_MEMBERS + 16 {
            return Err(invalid(
                "data-backup directory entry count exceeds its bound",
            ));
        }
    }
    Ok(entries)
}

pub(super) fn require_real_directory(
    path: &Path,
    description: &'static str,
) -> Result<fs::Metadata, DataRecoveryError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspecting data-backup directory", path, source))?;
    if !metadata.file_type().is_dir() {
        return Err(invalid(format!("{description} is not a real directory")));
    }
    Ok(metadata)
}

pub(super) fn require_regular_file(
    path: &Path,
    description: &'static str,
) -> Result<fs::Metadata, DataRecoveryError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspecting data-backup file", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(invalid(format!("{description} is not a regular file")));
    }
    Ok(metadata)
}

pub(super) fn create_private_directory(path: &Path) -> Result<(), DataRecoveryError> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(PRIVATE_DIRECTORY_MODE);
    builder
        .create(path)
        .map_err(|source| io_error("creating restored data directory", path, source))?;
    set_mode(
        path,
        PRIVATE_DIRECTORY_MODE,
        "protecting restored data directory",
    )
}

pub(super) fn set_mode(
    path: &Path,
    mode: u32,
    operation: &'static str,
) -> Result<(), DataRecoveryError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|source| io_error(operation, path, source))
}

pub(super) fn permission_mode(metadata: &fs::Metadata) -> u32 {
    metadata.permissions().mode() & 0o7777
}
