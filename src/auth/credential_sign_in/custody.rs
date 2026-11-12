// SPDX-License-Identifier: AGPL-3.0-only

use std::{fs, io::Read, os::unix::fs::MetadataExt, path::Path};
use rustix::{
    fs::{FileType, Mode, OFlags, fcntl_getfl, fcntl_setfl, fstat, open},
    io::Errno,
    process::getuid,
};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCustodyError { Read, UnsafeFile, IdentityChanged, TooLarge }

impl std::fmt::Display for FileCustodyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Read => "cannot read secret",
            Self::UnsafeFile => "secret file is unsafe",
            Self::IdentityChanged => "secret file changed while reading",
            Self::TooLarge => "secret exceeds the fixed size limit",
        })
    }
}

impl std::error::Error for FileCustodyError {}

pub fn read_credential_file(path: &Path, maximum: usize) -> Result<Zeroizing<Vec<u8>>, FileCustodyError> {
    read_credential_file_with_hooks(path, maximum, || {}, || {}, || {})
}

/// Hooks are at custody boundaries and cannot replace the reader or its data.
/// Classification precedes opening, and a changed entry refuses before reading.
pub fn read_credential_file_with_hooks(
    path: &Path,
    maximum: usize,
    before_open: impl FnOnce(),
    after_open: impl FnOnce(),
    after_read: impl FnOnce(),
) -> Result<Zeroizing<Vec<u8>>, FileCustodyError> {
    let initial = fs::symlink_metadata(path).map_err(|_| FileCustodyError::Read)?;
    if !safe_metadata(&initial) { return Err(FileCustodyError::UnsafeFile); }
    before_open();
    let descriptor = open(path, OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK, Mode::empty())
        .map_err(|error| if error == Errno::LOOP { FileCustodyError::UnsafeFile } else { FileCustodyError::Read })?;
    let opened = fstat(&descriptor).map_err(|_| FileCustodyError::Read)?;
    if !safe_stat(&opened) { return Err(FileCustodyError::UnsafeFile); }
    if !matches_stat(&initial, &opened) { return Err(FileCustodyError::IdentityChanged); }
    let flags = fcntl_getfl(&descriptor).map_err(|_| FileCustodyError::Read)?;
    fcntl_setfl(&descriptor, flags & !OFlags::NONBLOCK).map_err(|_| FileCustodyError::Read)?;
    after_open();
    let file: fs::File = descriptor.into();
    let capacity = maximum.checked_add(1).ok_or(FileCustodyError::TooLarge)?;
    let limit = u64::try_from(capacity).map_err(|_| FileCustodyError::TooLarge)?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
    (&file).take(limit).read_to_end(&mut bytes).map_err(|_| FileCustodyError::Read)?;
    if bytes.len() > maximum { return Err(FileCustodyError::TooLarge); }
    after_read();
    let after = fstat(&file).map_err(|_| FileCustodyError::Read)?;
    let current = fs::symlink_metadata(path).map_err(|_| FileCustodyError::IdentityChanged)?;
    if !safe_stat(&after) || !matches_stat(&initial, &after)
        || !safe_metadata(&current) || !matches_stat(&current, &after)
    {
        return Err(FileCustodyError::IdentityChanged);
    }
    Ok(bytes)
}

fn safe_metadata(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_file() && !metadata.file_type().is_symlink()
        && metadata.uid() == getuid().as_raw()
        && metadata.mode() & 0o077 == 0 && metadata.mode() & 0o400 != 0
}

fn safe_stat(stat: &rustix::fs::Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode).is_file() && stat.st_uid == getuid().as_raw()
        && stat.st_mode & 0o077 == 0 && stat.st_mode & 0o400 != 0
}

fn matches_stat(metadata: &fs::Metadata, stat: &rustix::fs::Stat) -> bool {
    metadata.dev() == stat.st_dev as u64 && metadata.ino() == stat.st_ino
        && metadata.uid() == stat.st_uid && metadata.mode() == stat.st_mode as u32
        && metadata.len() == stat.st_size as u64
        && metadata.mtime() == stat.st_mtime
        && metadata.mtime_nsec() == stat.st_mtime_nsec
}
