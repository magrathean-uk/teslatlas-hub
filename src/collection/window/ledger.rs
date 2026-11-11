// SPDX-License-Identifier: AGPL-3.0-only

//! Hash-chained append-only JSON-lines ledgers and atomic file helpers.
//!
//! Each line holds its number, the previous line's digest, the entry and its
//! own digest over the canonical form of the first three. Files are 0600 and
//! every append and replace is synced, with the directory synced after a
//! create or rename.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

pub const GENESIS_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    Missing,
    Broken,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Line<E> {
    pub n: u64,
    pub prev: String,
    pub entry: E,
    pub digest: String,
}

#[derive(Serialize)]
struct Unsealed<'a, E> {
    n: u64,
    prev: &'a str,
    entry: &'a E,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn seal<E: Serialize>(n: u64, prev: &str, entry: E) -> Result<Line<E>, LedgerError> {
    let canonical = serde_jcs::to_vec(&Unsealed {
        n,
        prev,
        entry: &entry,
    })
    .map_err(|_| LedgerError::Io)?;
    Ok(Line {
        n,
        prev: prev.to_owned(),
        digest: sha256_hex(&canonical),
        entry,
    })
}

/// Reads the whole ledger and verifies numbering, links and digests.
pub fn read<E: Serialize + DeserializeOwned>(path: &Path) -> Result<Vec<Line<E>>, LedgerError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(LedgerError::Missing);
        }
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            return Err(LedgerError::Broken);
        }
        Err(_) => return Err(LedgerError::Io),
    };
    if text.is_empty() || !text.ends_with('\n') {
        return Err(LedgerError::Broken);
    }
    let mut lines: Vec<Line<E>> = Vec::new();
    let mut prev = GENESIS_DIGEST.to_owned();
    for (index, raw) in text.lines().enumerate() {
        let line: Line<E> = serde_json::from_str(raw).map_err(|_| LedgerError::Broken)?;
        let expected = seal(index as u64 + 1, &prev, &line.entry)?;
        if line.n != expected.n || line.prev != prev || line.digest != expected.digest {
            return Err(LedgerError::Broken);
        }
        prev = line.digest.clone();
        lines.push(line);
    }
    Ok(lines)
}

fn encode<E: Serialize>(line: &Line<E>) -> Result<Vec<u8>, LedgerError> {
    let mut bytes = serde_json::to_vec(line).map_err(|_| LedgerError::Io)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn sync_parent(path: &Path) -> Result<(), LedgerError> {
    let parent = path.parent().ok_or(LedgerError::Io)?;
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| LedgerError::Io)
}

/// Creates a new ledger with its first entry; fails if the file exists.
pub fn create<E: Serialize>(path: &Path, first: E) -> Result<Line<E>, LedgerError> {
    let line = seal(1, GENESIS_DIGEST, first)?;
    let bytes = encode(&line)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| LedgerError::Io)?;
    file.write_all(&bytes).map_err(|_| LedgerError::Io)?;
    file.sync_all().map_err(|_| LedgerError::Io)?;
    sync_parent(path)?;
    Ok(line)
}

/// Verifies the chain, appends one line and syncs it.
pub fn append<E: Serialize + DeserializeOwned + Clone>(
    path: &Path,
    entry: E,
) -> Result<Line<E>, LedgerError> {
    let lines = read::<E>(path)?;
    let head = lines.last().ok_or(LedgerError::Broken)?;
    let line = seal(head.n + 1, &head.digest, entry)?;
    let bytes = encode(&line)?;
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|_| LedgerError::Io)?;
    file.write_all(&bytes).map_err(|_| LedgerError::Io)?;
    file.sync_all().map_err(|_| LedgerError::Io)?;
    Ok(line)
}

/// The temporary sibling `write_atomic` stages `path` in.
pub fn temp_path(path: &Path) -> PathBuf {
    path.with_extension("tmp")
}

/// Writes `bytes` to the temporary sibling of `path` (0600) and syncs it.
pub fn write_temp(path: &Path, bytes: &[u8]) -> Result<PathBuf, LedgerError> {
    let temporary = temp_path(path);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|_| LedgerError::Io)?;
    file.write_all(bytes).map_err(|_| LedgerError::Io)?;
    file.sync_all().map_err(|_| LedgerError::Io)?;
    Ok(temporary)
}

/// Replaces `path` atomically with `bytes` (0600), syncing file and directory.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), LedgerError> {
    let temporary = write_temp(path, bytes)?;
    fs::rename(&temporary, path).map_err(|_| LedgerError::Io)?;
    sync_parent(path)
}

/// Creates the directory (and parents) with mode 0700.
pub fn create_private_dir(path: &Path) -> Result<(), LedgerError> {
    fs::create_dir_all(path).map_err(|_| LedgerError::Io)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| LedgerError::Io)
}
