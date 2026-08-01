//! Durable, Hub-owned legacy Owner API token state for Linux.
//!
//! The imported systemd credential is bootstrap material only. Rotated values
//! live below `StateDirectory=teslatlas`, encrypted with narrowly derived keys
//! from the existing cursor key. The active pointer is independently
//! authenticated so a crash can expose only the old complete generation or the
//! new complete generation.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use rustix::fs::{FlockOperation, flock};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

pub const DEFAULT_STATE_ROOT: &str = "/var/lib/teslatlas/legacy-auth";
const MAX_STATE_BYTES: usize = 16 * 1024;
const GENERATION_PREFIX: &str = "generation-";
const GENERATION_SUFFIX: &str = ".bin";
const ACTIVE_FILE: &str = "active";
const LOCK_FILE: &str = ".refresh.lock";
const STATE_MAGIC: &[u8] = b"TAHLEG01";
const POINTER_MAGIC: &[u8] = b"TAHLEG-ACTIVE-1";
const NONCE_BYTES: usize = 12;
const HKDF_SALT: &[u8] = b"teslatlas-hub/legacy-token-state/v1";
const ENCRYPTION_INFO: &[u8] = b"aes-256-gcm generation";
const POINTER_INFO: &[u8] = b"hmac-sha256 active pointer";

#[derive(Clone)]
pub struct LegacyTokenState {
    root: PathBuf,
    encryption_key: [u8; 32],
    pointer_key: [u8; 32],
}

impl std::fmt::Debug for LegacyTokenState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LegacyTokenState")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

pub struct LegacyTokenStateLock {
    state: LegacyTokenState,
    _file: File,
}

pub struct LoadedLegacyTokenState {
    pub generation: Uuid,
    pub payload: Zeroizing<Vec<u8>>,
}

impl LegacyTokenState {
    pub fn new(cursor_key: [u8; 32]) -> Self {
        let prk = hmac_sha256(HKDF_SALT, &cursor_key);
        Self {
            root: PathBuf::from(DEFAULT_STATE_ROOT),
            encryption_key: hkdf_expand(&prk, ENCRYPTION_INFO),
            pointer_key: hkdf_expand(&prk, POINTER_INFO),
        }
    }

    pub fn lock(&self) -> Result<LegacyTokenStateLock, LegacyTokenStateError> {
        self.ensure_root()?;
        let path = self.root.join(LOCK_FILE);
        let file = open_private_file(&path, true)?;
        flock(&file, FlockOperation::LockExclusive).map_err(|source| LegacyTokenStateError::Io {
            operation: "lock legacy token state",
            path,
            source: source.into(),
        })?;
        Ok(LegacyTokenStateLock {
            state: self.clone(),
            _file: file,
        })
    }

    fn ensure_root(&self) -> Result<(), LegacyTokenStateError> {
        let parent = self.root.parent().ok_or(LegacyTokenStateError::InvalidRoot)?;
        ensure_private_directory(parent, false)?;
        match fs::create_dir(&self.root) {
            Ok(()) => fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
                .map_err(|source| LegacyTokenStateError::Io {
                    operation: "protect legacy token state directory",
                    path: self.root.clone(),
                    source,
                })?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(LegacyTokenStateError::Io {
                    operation: "create legacy token state directory",
                    path: self.root.clone(),
                    source,
                });
            }
        }
        ensure_private_directory(&self.root, true)
    }
}

impl LegacyTokenStateLock {
    /// A missing active pointer means this installation has not yet persisted a
    /// Hub-owned override. Any malformed override is an error, never a reason
    /// to fall back to the imported bootstrap credential.
    pub fn load(&mut self) -> Result<Option<LoadedLegacyTokenState>, LegacyTokenStateError> {
        let pointer_path = self.state.root.join(ACTIVE_FILE);
        let pointer = match read_private_file(&pointer_path, 256) {
            Ok(pointer) => pointer,
            Err(LegacyTokenStateError::NotFound(_)) => {
                self.reject_generation_artifacts_without_pointer()?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let generation = self.parse_pointer(&pointer)?;
        let generation_path = self.state.generation_path(generation);
        let envelope = read_private_file(&generation_path, MAX_STATE_BYTES + 128)?;
        let payload = self.decrypt_generation(generation, &envelope)?;
        self.cleanup_orphans(Some(generation))?;
        Ok(Some(LoadedLegacyTokenState {
            generation,
            payload,
        }))
    }

    pub fn persist(&mut self, payload: &[u8]) -> Result<Uuid, LegacyTokenStateError> {
        if payload.is_empty() || payload.len() > MAX_STATE_BYTES {
            return Err(LegacyTokenStateError::InvalidPayload);
        }
        let generation = Uuid::new_v4();
        let generation_path = self.state.generation_path(generation);
        let envelope = self.encrypt_generation(generation, payload)?;
        write_private_new_file(&generation_path, &envelope)?;
        sync_directory(&self.state.root)?;

        let pointer = self.encode_pointer(generation);
        let pointer_candidate = self
            .state
            .root
            .join(format!(".{ACTIVE_FILE}.{}.tmp", Uuid::new_v4()));
        let result = (|| {
            write_private_new_file(&pointer_candidate, &pointer)?;
            fs::rename(&pointer_candidate, self.state.root.join(ACTIVE_FILE)).map_err(|source| {
                LegacyTokenStateError::Io {
                    operation: "publish active legacy token generation",
                    path: self.state.root.join(ACTIVE_FILE),
                    source,
                }
            })?;
            sync_directory(&self.state.root)?;
            self.cleanup_orphans(Some(generation))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&pointer_candidate);
        }
        result.map(|()| generation)
    }

    fn encrypt_generation(
        &self,
        generation: Uuid,
        payload: &[u8],
    ) -> Result<Vec<u8>, LegacyTokenStateError> {
        let cipher = Aes256Gcm::new_from_slice(&self.state.encryption_key)
            .map_err(|_| LegacyTokenStateError::Crypto)?;
        let random = Uuid::new_v4();
        let nonce = &random.as_bytes()[..NONCE_BYTES];
        let encrypted = cipher
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: payload,
                    aad: generation.as_bytes(),
                },
            )
            .map_err(|_| LegacyTokenStateError::Crypto)?;
        let mut envelope = Vec::with_capacity(STATE_MAGIC.len() + NONCE_BYTES + encrypted.len());
        envelope.extend_from_slice(STATE_MAGIC);
        envelope.extend_from_slice(nonce);
        envelope.extend_from_slice(&encrypted);
        Ok(envelope)
    }

    fn decrypt_generation(
        &self,
        generation: Uuid,
        envelope: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, LegacyTokenStateError> {
        let header = STATE_MAGIC.len() + NONCE_BYTES;
        if envelope.len() <= header || !envelope.starts_with(STATE_MAGIC) {
            return Err(LegacyTokenStateError::InvalidGeneration);
        }
        let cipher = Aes256Gcm::new_from_slice(&self.state.encryption_key)
            .map_err(|_| LegacyTokenStateError::Crypto)?;
        let payload = cipher
            .decrypt(
                Nonce::from_slice(&envelope[STATE_MAGIC.len()..header]),
                Payload {
                    msg: &envelope[header..],
                    aad: generation.as_bytes(),
                },
            )
            .map_err(|_| LegacyTokenStateError::AuthenticationFailed)?;
        if payload.is_empty() || payload.len() > MAX_STATE_BYTES {
            return Err(LegacyTokenStateError::InvalidPayload);
        }
        Ok(Zeroizing::new(payload))
    }

    fn encode_pointer(&self, generation: Uuid) -> Vec<u8> {
        let generation = generation.to_string();
        let mut signed = Vec::with_capacity(POINTER_MAGIC.len() + 1 + generation.len());
        signed.extend_from_slice(POINTER_MAGIC);
        signed.push(b'\n');
        signed.extend_from_slice(generation.as_bytes());
        let tag = hmac_sha256(&self.state.pointer_key, &signed);
        signed.push(b'\n');
        signed.extend_from_slice(hex::encode(tag).as_bytes());
        signed.push(b'\n');
        signed
    }

    fn parse_pointer(&self, pointer: &[u8]) -> Result<Uuid, LegacyTokenStateError> {
        let pointer = std::str::from_utf8(pointer).map_err(|_| LegacyTokenStateError::InvalidPointer)?;
        let mut lines = pointer.split_terminator('\n');
        let (Some(magic), Some(generation), Some(tag), None) =
            (lines.next(), lines.next(), lines.next(), lines.next())
        else {
            return Err(LegacyTokenStateError::InvalidPointer);
        };
        if magic.as_bytes() != POINTER_MAGIC || generation.len() != 36 || tag.len() != 64 {
            return Err(LegacyTokenStateError::InvalidPointer);
        }
        let generation = Uuid::parse_str(generation).map_err(|_| LegacyTokenStateError::InvalidPointer)?;
        let supplied = hex::decode(tag).map_err(|_| LegacyTokenStateError::InvalidPointer)?;
        let mut signed = Vec::with_capacity(POINTER_MAGIC.len() + 1 + 36);
        signed.extend_from_slice(POINTER_MAGIC);
        signed.push(b'\n');
        signed.extend_from_slice(generation.to_string().as_bytes());
        if !constant_time_eq(&hmac_sha256(&self.state.pointer_key, &signed), &supplied) {
            return Err(LegacyTokenStateError::AuthenticationFailed);
        }
        Ok(generation)
    }

    fn cleanup_orphans(&self, active: Option<Uuid>) -> Result<(), LegacyTokenStateError> {
        let mut changed = false;
        for entry in fs::read_dir(&self.state.root).map_err(|source| LegacyTokenStateError::Io {
            operation: "list legacy token state directory",
            path: self.state.root.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| LegacyTokenStateError::Io {
                operation: "read legacy token state directory",
                path: self.state.root.clone(),
                source,
            })?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(generation) = parse_generation_name(&name) else { continue };
            let path = entry.path();
            ensure_private_regular_file(&path)?;
            if Some(generation) != active {
                fs::remove_file(&path).map_err(|source| LegacyTokenStateError::Io {
                    operation: "remove orphaned legacy token generation",
                    path,
                    source,
                })?;
                changed = true;
            }
        }
        if changed {
            sync_directory(&self.state.root)?;
        }
        Ok(())
    }

    fn reject_generation_artifacts_without_pointer(&self) -> Result<(), LegacyTokenStateError> {
        for entry in fs::read_dir(&self.state.root).map_err(|source| LegacyTokenStateError::Io {
            operation: "list legacy token state directory",
            path: self.state.root.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| LegacyTokenStateError::Io {
                operation: "read legacy token state directory",
                path: self.state.root.clone(),
                source,
            })?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(GENERATION_PREFIX)
            {
                return Err(LegacyTokenStateError::GenerationWithoutPointer);
            }
        }
        Ok(())
    }
}

impl LegacyTokenState {
    fn generation_path(&self, generation: Uuid) -> PathBuf {
        self.root
            .join(format!("{GENERATION_PREFIX}{generation}{GENERATION_SUFFIX}"))
    }
}

fn parse_generation_name(name: &str) -> Option<Uuid> {
    let generation = name.strip_prefix(GENERATION_PREFIX)?.strip_suffix(GENERATION_SUFFIX)?;
    Uuid::parse_str(generation).ok()
}

fn open_private_file(path: &Path, create: bool) -> Result<File, LegacyTokenStateError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create {
        options.create(true).mode(0o600);
    }
    let file = options.open(path).map_err(|source| LegacyTokenStateError::Io {
        operation: "open legacy token state file",
        path: path.to_path_buf(),
        source,
    })?;
    ensure_private_regular_file(path)?;
    Ok(file)
}

fn write_private_new_file(path: &Path, bytes: &[u8]) -> Result<(), LegacyTokenStateError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| LegacyTokenStateError::Io {
            operation: "create legacy token state file",
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| LegacyTokenStateError::Io {
        operation: "write legacy token state file",
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| LegacyTokenStateError::Io {
        operation: "sync legacy token state file",
        path: path.to_path_buf(),
        source,
    })
}

fn read_private_file(path: &Path, maximum: usize) -> Result<Vec<u8>, LegacyTokenStateError> {
    ensure_private_regular_file(path)?;
    let mut file = File::open(path).map_err(|source| LegacyTokenStateError::Io {
        operation: "open legacy token state file",
        path: path.to_path_buf(),
        source,
    })?;
    let before = file.metadata().map_err(|source| LegacyTokenStateError::Io {
        operation: "inspect legacy token state file",
        path: path.to_path_buf(),
        source,
    })?;
    ensure_private_regular_file_metadata(&before, path)?;
    let mut bytes = Vec::with_capacity(maximum.saturating_add(1));
    Read::by_ref(&mut file)
        .take(maximum.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| LegacyTokenStateError::Io {
            operation: "read legacy token state file",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > maximum {
        return Err(LegacyTokenStateError::InvalidPayload);
    }
    let after = file.metadata().map_err(|source| LegacyTokenStateError::Io {
        operation: "inspect legacy token state file",
        path: path.to_path_buf(),
        source,
    })?;
    ensure_private_regular_file_metadata(&after, path)?;
    if before.dev() != after.dev() || before.ino() != after.ino() {
        return Err(LegacyTokenStateError::ChangedWhileReading);
    }
    Ok(bytes)
}

fn ensure_private_directory(path: &Path, root: bool) -> Result<(), LegacyTokenStateError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| LegacyTokenStateError::Io {
        operation: "inspect legacy token state directory",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LegacyTokenStateError::UnsafePath);
    }
    if root && metadata.permissions().mode() & 0o077 != 0 {
        return Err(LegacyTokenStateError::UnsafePermissions);
    }
    if !root && metadata.permissions().mode() & 0o027 != 0 {
        return Err(LegacyTokenStateError::UnsafePermissions);
    }
    Ok(())
}

fn ensure_private_regular_file(path: &Path) -> Result<(), LegacyTokenStateError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => LegacyTokenStateError::NotFound(path.to_path_buf()),
        _ => LegacyTokenStateError::Io {
            operation: "inspect legacy token state file",
            path: path.to_path_buf(),
            source,
        },
    })?;
    ensure_private_regular_file_metadata(&metadata, path)
}

fn ensure_private_regular_file_metadata(
    metadata: &fs::Metadata,
    _path: &Path,
) -> Result<(), LegacyTokenStateError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(LegacyTokenStateError::UnsafePath);
    }
    if metadata.permissions().mode() & 0o777 != 0o600 {
        return Err(LegacyTokenStateError::UnsafePermissions);
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), LegacyTokenStateError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| LegacyTokenStateError::Io {
            operation: "sync legacy token state directory",
            path: path.to_path_buf(),
            source,
        })
}

// HKDF-SHA256 (RFC 5869) with one 32-byte output block per purpose. Keeping
// this local avoids exposing or directly reusing the cursor signing key.
fn hkdf_expand(prk: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(info.len() + 1);
    input.extend_from_slice(info);
    input.push(1);
    hmac_sha256(prk, &input)
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_BYTES: usize = 64;
    let mut key_block = [0_u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(key_block.map(|byte| byte ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(key_block.map(|byte| byte ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter().zip(right).fold(0_u8, |different, (left, right)| {
        different | (left ^ right)
    }) == 0
}

#[derive(Debug, Error)]
pub enum LegacyTokenStateError {
    #[error("legacy token state root is invalid")]
    InvalidRoot,
    #[error("legacy token state path is unsafe")]
    UnsafePath,
    #[error("legacy token state permissions are unsafe")]
    UnsafePermissions,
    #[error("legacy token state file is missing")]
    NotFound(PathBuf),
    #[error("legacy token state changed while it was read")]
    ChangedWhileReading,
    #[error("legacy token state pointer is malformed")]
    InvalidPointer,
    #[error("legacy token state has a generation without an active pointer")]
    GenerationWithoutPointer,
    #[error("legacy token state generation is malformed")]
    InvalidGeneration,
    #[error("legacy token state authentication failed")]
    AuthenticationFailed,
    #[error("legacy token state payload is invalid")]
    InvalidPayload,
    #[error("legacy token state encryption failed")]
    Crypto,
    #[error("legacy token state I/O failed")]
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}
