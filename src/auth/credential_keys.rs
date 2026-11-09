// SPDX-License-Identifier: AGPL-3.0-only

//! C10 key custody. Store/vault admission, row barriers and reservation
//! transactions belong to their owning parts. In particular, callers may run
//! `finish_rotation` only after the re-seal commit's barrier and previous-id scan.

use std::{
    fmt,
    fs::File,
    io::{self, Read, Write},
    os::fd::AsFd,
    path::{Path, PathBuf},
    time::Duration,
};

use rustix::fs::{Mode, fchmod, fstat};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::auth::credential_envelope::{EnvelopeKeys, KeyId, derive_envelope_key};
use crate::platform::private_directory::{
    EntryState, PrivateDirectory, PrivateDirectoryError, UnsafeEntry, clear_extended_acl,
    durable_sync_fd,
};

pub(crate) const KEYRING_MAGIC: [u8; 4] = *b"HCKR";
pub(crate) const KEYRING_VERSION: u8 = 1;
pub(crate) const KEYRING_BYTES: usize = 88;
pub(crate) const ROTATING_KEYRING_BYTES: usize = 136;
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "Part K handoff API; consumed by later parts.")
)]
pub(crate) const FORCED_ROTATION_INVOCATIONS: u64 = 1 << 31;
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part K backup-exclusion handoff; production integration belongs to V1."
    )
)]
pub(crate) const TMUTIL_RUN_LIMIT: Duration = Duration::from_secs(10);
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part K backup-exclusion handoff; production integration belongs to V1."
    )
)]
pub(crate) const TMUTIL_EXECUTABLE: &str = "/usr/bin/tmutil";
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part K backup-exclusion handoff; production integration belongs to V1."
    )
)]
pub(crate) const TMUTIL_OUTPUT_CAP: usize = 65_536;
pub(crate) const KEYRING_NAME: &str = "credential-keys";
const KEY_RECORD_BYTES: usize = 48;
const CHECKSUM_BYTES: usize = 32;
const KEYRING_HEADER_BYTES: usize = 8;

pub(crate) struct KeyMaterial {
    pub id: KeyId,
    pub created_at_s: u64,
    pub key: Zeroizing<[u8; 32]>,
}

pub(crate) struct Keyring {
    pub current: KeyMaterial,
    pub previous: Option<KeyMaterial>,
}

/// Roots, derived keys and encoded/read buffers are wiped by their owners.
/// Deliberately no `Clone`: the vault shares one handle behind its lock.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "Part K handoff API; consumed by later parts.")
)]
pub(crate) struct KeyringHandle {
    keyring: Keyring,
    pub exposure_suspected: bool,
    current_derived: Zeroizing<[u8; 32]>,
    previous_derived: Option<Zeroizing<[u8; 32]>>,
}

impl KeyringHandle {
    pub(crate) fn new(keyring: Keyring) -> Self {
        event("derive_handle");
        let current_derived = derive_envelope_key(&keyring.current.key);
        let previous_derived = keyring
            .previous
            .as_ref()
            .map(|key| derive_envelope_key(&key.key));
        Self {
            keyring,
            exposure_suspected: false,
            current_derived,
            previous_derived,
        }
    }

    /// Immutable access keeps root metadata and the cached derived keys aligned.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "Part K handoff API; consumed by later parts.")
    )]
    pub(crate) fn keyring(&self) -> &Keyring {
        &self.keyring
    }
}

impl EnvelopeKeys for KeyringHandle {
    fn envelope_key(&self, id: KeyId) -> Option<&Zeroizing<[u8; 32]>> {
        if id == self.keyring.current.id {
            return Some(&self.current_derived);
        }
        self.keyring
            .previous
            .as_ref()
            .filter(|previous| previous.id == id)
            .and(self.previous_derived.as_ref())
    }
}

impl fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyMaterial")
            .field("id_present", &true)
            .field("created_at_s", &self.created_at_s)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Keyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keyring")
            .field("current", &self.current)
            .field("previous", &self.previous)
            .finish()
    }
}

impl fmt::Debug for KeyringHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyringHandle")
            .field("keyring", &self.keyring)
            .field("exposure_suspected", &self.exposure_suspected)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnsafeReason {
    Symlink,
    NotRegular,
    Owner,
    LinkCount,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CredentialKeyError {
    KeyringMissing,
    KeyringUnreadable,
    KeyringUnsafe(UnsafeReason),
    KeyringInvalid,
    KeyringUnsupportedVersion(u8),
    EntropyUnavailable,
    DurabilityUnavailable,
    BackupExclusionFailed,
    Io { operation: &'static str },
}

impl fmt::Display for CredentialKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CredentialKeyError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExclusionState {
    Verified,
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "Only the non-macOS production adapter constructs this platform state."
        )
    )]
    NotApplicable,
}

pub(crate) trait BackupExclusion {
    fn ensure_excluded(&self, secrets_path: &Path) -> Result<ExclusionState, CredentialKeyError>;
}

/// R23's source distinction. Selection/configuration and Linux runtime proof
/// belong to G2-9; a read-only source is never passed to writable file helpers.
#[allow(
    dead_code,
    reason = "R23 handoff structure; Linux/Docker selection is G2-9."
)]
pub(crate) enum KeyringSource<'a> {
    Writable(&'a PrivateDirectory),
    ReadOnly(PathBuf),
}

pub(crate) fn encode_keyring(k: &Keyring) -> Zeroizing<Vec<u8>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(if k.previous.is_some() {
        ROTATING_KEYRING_BYTES
    } else {
        KEYRING_BYTES
    }));
    bytes.extend_from_slice(&KEYRING_MAGIC);
    bytes.extend_from_slice(&[KEYRING_VERSION, u8::from(k.previous.is_some()), 0, 0]);
    encode_material(&mut bytes, &k.current);
    if let Some(previous) = &k.previous {
        encode_material(&mut bytes, previous);
    }
    let checksum = Sha256::digest(bytes.as_slice());
    bytes.extend_from_slice(&checksum);
    bytes
}

fn encode_material(bytes: &mut Vec<u8>, key: &KeyMaterial) {
    bytes.extend_from_slice(&key.id.0);
    bytes.extend_from_slice(&key.created_at_s.to_be_bytes());
    bytes.extend_from_slice(key.key.as_ref());
}

pub(crate) fn decode_keyring(bytes: &[u8]) -> Result<Keyring, CredentialKeyError> {
    if !matches!(bytes.len(), KEYRING_BYTES | ROTATING_KEYRING_BYTES) || bytes[..4] != KEYRING_MAGIC
    {
        return Err(CredentialKeyError::KeyringInvalid);
    }
    if bytes[4] != KEYRING_VERSION {
        return Err(CredentialKeyError::KeyringUnsupportedVersion(bytes[4]));
    }
    let rotating = bytes[5] == 1;
    if bytes[5] & !1 != 0
        || bytes[6..8] != [0, 0]
        || bytes.len()
            != if rotating {
                ROTATING_KEYRING_BYTES
            } else {
                KEYRING_BYTES
            }
    {
        return Err(CredentialKeyError::KeyringInvalid);
    }
    let content_end = bytes.len() - CHECKSUM_BYTES;
    if Sha256::digest(&bytes[..content_end]).as_slice() != &bytes[content_end..] {
        return Err(CredentialKeyError::KeyringInvalid);
    }
    let current =
        decode_material(&bytes[KEYRING_HEADER_BYTES..KEYRING_HEADER_BYTES + KEY_RECORD_BYTES]);
    let previous = rotating
        .then(|| decode_material(&bytes[KEYRING_HEADER_BYTES + KEY_RECORD_BYTES..content_end]));
    if previous.as_ref().is_some_and(|key| key.id == current.id) {
        return Err(CredentialKeyError::KeyringInvalid);
    }
    Ok(Keyring { current, previous })
}

fn decode_material(bytes: &[u8]) -> KeyMaterial {
    let mut key = Zeroizing::new([0; 32]);
    key.copy_from_slice(&bytes[16..48]);
    KeyMaterial {
        id: KeyId(bytes[..8].try_into().expect("validated key record")),
        created_at_s: u64::from_be_bytes(bytes[8..16].try_into().expect("validated key record")),
        key,
    }
}

/// R68 metadata only: no key bytes or derived-key handle escape the check.
#[derive(Debug)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "Part K handoff API; consumed by later parts.")
)]
pub(crate) struct KeyringCheck {
    pub current: KeyMetadata,
    pub previous: Option<KeyMetadata>,
    pub exposure_suspected: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "Part K handoff API; consumed by later parts.")
)]
pub(crate) struct KeyMetadata {
    pub id: KeyId,
    pub created_at_s: u64,
}

/// Non-mutating R16/R12 check for status, diagnostics and read-only callers.
/// Temporary entries, permissions and ACLs are left exactly as found; no
/// barrier or tmutil runs and no `KeyringHandle` is constructed. On macOS the
/// retained ACL verifier may spawn a read-only listing process (M2P-37).
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "Part K handoff API; consumed by later parts.")
)]
pub(crate) fn check_keyring(
    secrets: &PrivateDirectory,
) -> Result<Option<KeyringCheck>, CredentialKeyError> {
    Ok(
        read_keyring(secrets, false)?.map(|(keyring, exposure_suspected)| {
            let metadata = |key: &KeyMaterial| KeyMetadata {
                id: key.id,
                created_at_s: key.created_at_s,
            };
            KeyringCheck {
                current: metadata(&keyring.current),
                previous: keyring.previous.as_ref().map(metadata),
                exposure_suspected,
            }
        }),
    )
}

/// Repairing load for the admitted writable vault only. The caller holds the
/// data-directory/vault lock, so cleanup cannot remove an active publication.
/// Status, doctor, diagnostics and read-only callers use `check_keyring`.
/// No absent/invalid/unreadable/unsafe outcome creates a replacement; the
/// caller separately owns R17's knowledge of whether credential rows exist.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "Part K handoff API; consumed by later parts.")
)]
pub(crate) fn load_keyring(
    secrets: &PrivateDirectory,
) -> Result<Option<KeyringHandle>, CredentialKeyError> {
    cleanup_temporary_files(secrets)?;
    Ok(
        read_keyring(secrets, true)?.map(|(keyring, exposure_suspected)| {
            let mut handle = KeyringHandle::new(keyring);
            handle.exposure_suspected = exposure_suspected;
            handle
        }),
    )
}

fn read_keyring(
    secrets: &PrivateDirectory,
    repair: bool,
) -> Result<Option<(Keyring, bool)>, CredentialKeyError> {
    let descriptor = match secrets
        .inspect_entry(KEYRING_NAME)
        .map_err(directory_error)?
    {
        EntryState::Absent => return Ok(None),
        EntryState::Unsafe(UnsafeEntry::Symlink) => {
            return Err(CredentialKeyError::KeyringUnsafe(UnsafeReason::Symlink));
        }
        EntryState::Unsafe(UnsafeEntry::NotRegular) => {
            return Err(CredentialKeyError::KeyringUnsafe(UnsafeReason::NotRegular));
        }
        EntryState::Regular(descriptor) => descriptor,
    };
    let facts = fstat(&descriptor).map_err(|error| read_error(error.into(), "stat"))?;
    let owner = facts.st_uid;
    #[cfg(test)]
    let owner = KEY_FAULTS.with(|state| state.borrow().owner.unwrap_or(owner));
    if owner != rustix::process::geteuid().as_raw() {
        return Err(CredentialKeyError::KeyringUnsafe(UnsafeReason::Owner));
    }
    if facts.st_nlink != 1 {
        return Err(CredentialKeyError::KeyringUnsafe(UnsafeReason::LinkCount));
    }
    if !matches!(facts.st_size, 88 | 136) {
        return Err(CredentialKeyError::KeyringInvalid);
    }
    let path = secrets.path().join(KEYRING_NAME);
    let exposure_suspected = facts.st_mode & 0o077 != 0;
    #[cfg(target_os = "macos")]
    let mut exposure_suspected = exposure_suspected;
    if repair && exposure_suspected {
        event("chmod");
        fchmod(&descriptor, Mode::RUSR | Mode::WUSR)
            .map_err(|error| read_error(error.into(), "chmod"))?;
    }
    #[cfg(target_os = "macos")]
    {
        use crate::platform::private_directory::verify_no_extended_acl;
        event("acl_check");
        match verify_no_extended_acl(&descriptor, &path) {
            Ok(()) => {}
            Err(PrivateDirectoryError::WritableByOthers(_)) => {
                exposure_suspected = true;
                if repair {
                    event("chmod");
                    fchmod(&descriptor, Mode::RUSR | Mode::WUSR)
                        .map_err(|error| read_error(error.into(), "chmod"))?;
                    event("clear_acl");
                    #[cfg(test)]
                    let fail_clear = KEY_FAULTS.with(|state| state.borrow().acl_clear_fail);
                    #[cfg(not(test))]
                    let fail_clear = false;
                    let cleared = if fail_clear {
                        Err(PrivateDirectoryError::Io {
                            path: path.clone(),
                            source: io::Error::from_raw_os_error(
                                rustix::io::Errno::IO.raw_os_error(),
                            ),
                        })
                    } else {
                        clear_extended_acl(&descriptor, &path)
                    };
                    cleared.map_err(|_| CredentialKeyError::Io {
                        operation: "clear_acl",
                    })?;
                }
            }
            Err(_) => {
                return Err(CredentialKeyError::Io {
                    operation: "check_acl",
                });
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    if repair {
        // A POSIX ACL's effective group mask is already reflected in mode.
        clear_extended_acl(&descriptor, &path).map_err(|_| CredentialKeyError::Io {
            operation: "clear_acl",
        })?;
    }
    // The bounded read also detects a file expanded after the descriptor's
    // size check. Every byte lands directly in a zeroising buffer.
    let file = File::from(descriptor);
    let mut bytes = Zeroizing::new(Vec::with_capacity(ROTATING_KEYRING_BYTES + 1));
    event("read");
    file.take((ROTATING_KEYRING_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| read_error(error, "read"))?;
    Ok(Some((decode_keyring(&bytes)?, exposure_suspected)))
}

fn read_error(error: io::Error, operation: &'static str) -> CredentialKeyError {
    if matches!(
        error.raw_os_error(),
        Some(code) if code == rustix::io::Errno::ACCESS.raw_os_error()
            || code == rustix::io::Errno::IO.raw_os_error()
    ) {
        CredentialKeyError::KeyringUnreadable
    } else {
        CredentialKeyError::Io { operation }
    }
}

fn directory_error(error: PrivateDirectoryError) -> CredentialKeyError {
    match error {
        PrivateDirectoryError::Io { source, .. } => read_error(source, "inspect"),
        _ => CredentialKeyError::KeyringUnsafe(UnsafeReason::Directory),
    }
}

fn temporary_name(name: &str) -> bool {
    let Some(uuid) = name
        .strip_prefix(".credential-keys-")
        .and_then(|text| text.strip_suffix(".tmp"))
        .and_then(|text| Uuid::parse_str(text).ok())
    else {
        return false;
    };
    uuid.get_version_num() == 4
        && uuid.get_variant() == uuid::Variant::RFC4122
        && name == format!(".credential-keys-{}.tmp", uuid.hyphenated())
}

fn cleanup_temporary_files(secrets: &PrivateDirectory) -> Result<(), CredentialKeyError> {
    // A directory planted under this private temporary-file pattern is not
    // recursively removed. unlinkat refuses it and the repairing load fails
    // closed until the admitted owner repairs that residue (K-OR-12).
    let mut removed = false;
    for name in secrets.entry_names().map_err(|_| CredentialKeyError::Io {
        operation: "list_temporary",
    })? {
        if temporary_name(&name) {
            // unlinkat removes the entry itself, including a planted link;
            // it never reads or follows the target and cannot remove a directory.
            secrets.remove(&name).map_err(|_| CredentialKeyError::Io {
                operation: "remove_temporary",
            })?;
            removed = true;
        }
    }
    if removed {
        directory_barrier(secrets)?;
    }
    Ok(())
}

fn ensure_backup(
    secrets: &PrivateDirectory,
    exclusion: &dyn BackupExclusion,
) -> Result<(), CredentialKeyError> {
    event("exclusion");
    let result = exclusion
        .ensure_excluded(secrets.path())
        .map_err(|_| CredentialKeyError::BackupExclusionFailed)?;
    if cfg!(target_os = "macos") && result != ExclusionState::Verified {
        return Err(CredentialKeyError::BackupExclusionFailed);
    }
    Ok(())
}

fn random_fill(bytes: &mut [u8]) -> Result<(), CredentialKeyError> {
    #[cfg(test)]
    {
        let supplied = KEY_FAULTS.with(|state| {
            let mut state = state.borrow_mut();
            if state.entropy_fail {
                return Some(Err(CredentialKeyError::EntropyUnavailable));
            }
            state.entropy.as_mut().and_then(|values| {
                (!values.is_empty()).then(|| {
                    let value = values.remove(0);
                    if value.len() != bytes.len() {
                        Err(CredentialKeyError::EntropyUnavailable)
                    } else {
                        bytes.copy_from_slice(&value);
                        Ok(())
                    }
                })
            })
        });
        if let Some(result) = supplied {
            return result;
        }
    }
    getrandom::fill(bytes).map_err(|_| CredentialKeyError::EntropyUnavailable)
}

fn random_material(now_s: u64, forbidden_ids: &[KeyId]) -> Result<KeyMaterial, CredentialKeyError> {
    let mut key = Zeroizing::new([0; 32]);
    random_fill(key.as_mut())?;
    let mut id = [0; 8];
    loop {
        random_fill(&mut id)?;
        if !forbidden_ids.contains(&KeyId(id)) {
            break;
        }
    }
    Ok(KeyMaterial {
        id: KeyId(id),
        created_at_s: now_s,
        key,
    })
}

fn new_temporary_name() -> Result<String, CredentialKeyError> {
    let mut bytes = [0; 16];
    random_fill(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(".credential-keys-{}.tmp", Uuid::from_bytes(bytes)))
}

fn file_barrier(file: &File) -> Result<(), CredentialKeyError> {
    event("file_barrier");
    #[cfg(test)]
    if KEY_FAULTS.with(|state| state.borrow().file_barrier_fail) {
        return Err(CredentialKeyError::DurabilityUnavailable);
    }
    durable_sync_fd(file.as_fd()).map_err(|_| CredentialKeyError::DurabilityUnavailable)
}

fn directory_barrier(secrets: &PrivateDirectory) -> Result<(), CredentialKeyError> {
    event("directory_barrier");
    #[cfg(test)]
    if KEY_FAULTS.with(|state| state.borrow().directory_barrier_fail) {
        return Err(CredentialKeyError::DurabilityUnavailable);
    }
    secrets
        .durable_sync()
        .map_err(|_| CredentialKeyError::DurabilityUnavailable)
}

fn publish(secrets: &PrivateDirectory, keyring: &Keyring) -> Result<(), CredentialKeyError> {
    let name = new_temporary_name()?;
    event("create");
    let mut file = secrets
        .create_exclusive(&name)
        .map_err(|_| CredentialKeyError::Io {
            operation: "create_temporary",
        })?;
    let before_rename = (|| {
        event("write");
        #[cfg(test)]
        if KEY_FAULTS.with(|state| state.borrow().write_errno.is_some()) {
            return Err(CredentialKeyError::Io { operation: "write" });
        }
        let bytes = encode_keyring(keyring);
        file.write_all(&bytes)
            .map_err(|_| CredentialKeyError::Io { operation: "write" })?;
        file_barrier(&file)?;
        #[cfg(test)]
        if KEY_FAULTS.with(|state| state.borrow().rename_errno.is_some()) {
            return Err(CredentialKeyError::Io {
                operation: "rename",
            });
        }
        event("rename");
        secrets
            .rename(&name, KEYRING_NAME)
            .map_err(|_| CredentialKeyError::Io {
                operation: "rename",
            })
    })();
    if let Err(error) = before_rename {
        // A cleanup failure must never replace the write/barrier/rename error.
        // Its private temporary remains for the next admitted writable load.
        event("cleanup_temporary");
        #[cfg(test)]
        let cleanup_fails = KEY_FAULTS.with(|state| state.borrow().cleanup_remove_fail);
        #[cfg(not(test))]
        let cleanup_fails = false;
        let removed = if cleanup_fails {
            Err(io::Error::from_raw_os_error(
                rustix::io::Errno::IO.raw_os_error(),
            ))
        } else {
            secrets.remove(&name)
        };
        let _ = removed;
        return Err(error);
    }
    // The name now belongs to the new keyring. A failed second barrier must
    // leave it in place, unproven, for the next admitted vault open.
    directory_barrier(secrets)?;
    event("published");
    Ok(())
}

/// Requires no existing keyring under the caller's admitted lock. The logical
/// `Io { operation: "keyring_exists" }` is a caller-contract refusal, not a
/// damaged-file diagnosis or an operating-system I/O failure.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "Part K handoff API; consumed by later parts.")
)]
pub(crate) fn create_keyring(
    secrets: &PrivateDirectory,
    exclusion: &dyn BackupExclusion,
    now_s: u64,
) -> Result<KeyringHandle, CredentialKeyError> {
    if load_keyring(secrets)?.is_some() {
        return Err(CredentialKeyError::Io {
            operation: "keyring_exists",
        });
    }
    ensure_backup(secrets, exclusion)?;
    let keyring = Keyring {
        current: random_material(now_s, &[])?,
        previous: None,
    };
    publish(secrets, &keyring)?;
    Ok(KeyringHandle::new(keyring))
}

fn copy_material(key: &KeyMaterial) -> KeyMaterial {
    let mut root = Zeroizing::new([0; 32]);
    root.copy_from_slice(key.key.as_ref());
    KeyMaterial {
        id: key.id,
        created_at_s: key.created_at_s,
        key: root,
    }
}

/// A valid file that differs from the caller's snapshot is a stale-handle
/// caller-contract refusal (`keyring_changed`), not a damaged keyring.
fn ensure_current(
    secrets: &PrivateDirectory,
    current: &KeyringHandle,
) -> Result<bool, CredentialKeyError> {
    let on_disk = load_keyring(secrets)?.ok_or(CredentialKeyError::KeyringMissing)?;
    if encode_keyring(&on_disk.keyring).as_slice() != encode_keyring(&current.keyring).as_slice() {
        return Err(CredentialKeyError::Io {
            operation: "keyring_changed",
        });
    }
    Ok(on_disk.exposure_suspected)
}

/// R21 step 1 only. A rotation already in progress must resume at its row
/// step, rather than overwriting the previous key that those rows still need.
/// `Io { operation: "rotation_in_progress" }` is a caller-contract refusal.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "Part K handoff API; consumed by later parts.")
)]
pub(crate) fn begin_rotation(
    secrets: &PrivateDirectory,
    exclusion: &dyn BackupExclusion,
    current: &KeyringHandle,
    now_s: u64,
) -> Result<KeyringHandle, CredentialKeyError> {
    ensure_backup(secrets, exclusion)?;
    if current.keyring.previous.is_some() {
        return Err(CredentialKeyError::Io {
            operation: "rotation_in_progress",
        });
    }
    let reloaded_exposure = ensure_current(secrets, current)?;
    let mut forbidden = vec![current.keyring.current.id];
    if let Some(previous) = &current.keyring.previous {
        forbidden.push(previous.id);
    }
    let keyring = Keyring {
        current: random_material(now_s, &forbidden)?,
        previous: Some(copy_material(&current.keyring.current)),
    };
    publish(secrets, &keyring)?;
    let mut handle = KeyringHandle::new(keyring);
    handle.exposure_suspected = current.exposure_suspected || reloaded_exposure;
    Ok(handle)
}

/// R21 step 3 only. The vault must already have proven step 2 durable and
/// verified no stored envelope carries the previous id, under its held lock.
/// `Io { operation: "rotation_not_started" }` is a caller-contract refusal.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "Part K handoff API; consumed by later parts.")
)]
pub(crate) fn finish_rotation(
    secrets: &PrivateDirectory,
    rotating: &KeyringHandle,
) -> Result<KeyringHandle, CredentialKeyError> {
    if rotating.keyring.previous.is_none() {
        return Err(CredentialKeyError::Io {
            operation: "rotation_not_started",
        });
    }
    let reloaded_exposure = ensure_current(secrets, rotating)?;
    let keyring = Keyring {
        current: copy_material(&rotating.keyring.current),
        previous: None,
    };
    publish(secrets, &keyring)?;
    let mut handle = KeyringHandle::new(keyring);
    handle.exposure_suspected = rotating.exposure_suspected || reloaded_exposure;
    Ok(handle)
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part K backup-exclusion handoff; production integration belongs to V1."
    )
)]
pub(crate) struct TmutilOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub timed_out: bool,
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part K backup-exclusion handoff; production integration belongs to V1."
    )
)]
pub(crate) trait TmutilRunner {
    fn run(
        &self,
        operation: &str,
        path: &Path,
        limit: Duration,
    ) -> Result<TmutilOutput, CredentialKeyError>;
}

/// Recording runners exercise this macOS helper without changing Time Machine
/// state. The platform adapter below chooses NotApplicable off macOS.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part K backup-exclusion handoff; production integration belongs to V1."
    )
)]
pub(crate) struct TmutilExclusion<R: TmutilRunner>(pub R);

impl<R: TmutilRunner> BackupExclusion for TmutilExclusion<R> {
    fn ensure_excluded(&self, path: &Path) -> Result<ExclusionState, CredentialKeyError> {
        let added = self
            .0
            .run("addexclusion", path, TMUTIL_RUN_LIMIT)
            .map_err(|_| CredentialKeyError::BackupExclusionFailed)?;
        if !added.success || added.timed_out {
            return Err(CredentialKeyError::BackupExclusionFailed);
        }
        let checked = self
            .0
            .run("isexcluded", path, TMUTIL_RUN_LIMIT)
            .map_err(|_| CredentialKeyError::BackupExclusionFailed)?;
        if !checked.success
            || checked.timed_out
            || !checked
                .stdout
                .split(|byte| *byte == b'\n')
                .any(|line| line.starts_with(b"[Excluded]"))
        {
            return Err(CredentialKeyError::BackupExclusionFailed);
        }
        Ok(ExclusionState::Verified)
    }
}

#[allow(
    dead_code,
    reason = "Production exclusion adapter for the later vault; tests select an explicit fake runner."
)]
pub(crate) struct SystemBackupExclusion;
impl BackupExclusion for SystemBackupExclusion {
    fn ensure_excluded(&self, path: &Path) -> Result<ExclusionState, CredentialKeyError> {
        #[cfg(target_os = "macos")]
        {
            TmutilExclusion(SystemTmutil).ensure_excluded(path)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Ok(ExclusionState::NotApplicable)
        }
    }
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part K backup-exclusion handoff; production integration belongs to V1."
    )
)]
pub(crate) struct SystemTmutil;
impl TmutilRunner for SystemTmutil {
    fn run(
        &self,
        operation: &str,
        path: &Path,
        _limit: Duration,
    ) -> Result<TmutilOutput, CredentialKeyError> {
        #[cfg(test)]
        {
            // No default executor exists in a test build. A scoped test may
            // select only a temporary fake, never the real tmutil executable.
            let settings = TMUTIL_TEST_SETTINGS.with(|state| state.borrow().clone());
            let settings = settings.ok_or(CredentialKeyError::BackupExclusionFailed)?;
            run_tmutil(&settings.program, operation, path, settings.limit)
        }
        #[cfg(not(test))]
        {
            // Neither the trait argument nor configuration can change these.
            run_tmutil(
                Path::new(TMUTIL_EXECUTABLE),
                operation,
                path,
                TMUTIL_RUN_LIMIT,
            )
        }
    }
}

/// One real subprocess deadline/pipe/reap implementation in every build.
/// Only the production adapter selects the system program; cfg(test) selects
/// an explicitly scoped fake executable inside the test temporary directory.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part K backup-exclusion handoff; production integration belongs to V1."
    )
)]
fn run_tmutil(
    program: &Path,
    operation: &str,
    path: &Path,
    limit: Duration,
) -> Result<TmutilOutput, CredentialKeyError> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    #[cfg(test)]
    use std::os::unix::process::ExitStatusExt;
    use std::{
        process::{Command, Stdio},
        time::Instant,
    };
    if !matches!(operation, "addexclusion" | "isexcluded") {
        return Err(CredentialKeyError::BackupExclusionFailed);
    }
    #[cfg(test)]
    if !TMUTIL_TEST_SETTINGS.with(|state| {
        state
            .borrow()
            .as_ref()
            .is_some_and(|settings| settings.program == program && settings.limit == limit)
    }) {
        return Err(CredentialKeyError::BackupExclusionFailed);
    }
    let start = Instant::now();
    let mut child = Command::new(program)
        .arg(operation)
        .arg(path)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| CredentialKeyError::BackupExclusionFailed)?;
    #[cfg(test)]
    let mut observation = TmutilObservation {
        program: program.to_path_buf(),
        operation: operation.to_owned(),
        pid: child.id(),
        ..Default::default()
    };
    let result = (|| {
        let mut stdout = child
            .stdout
            .take()
            .ok_or(CredentialKeyError::BackupExclusionFailed)?;
        let flags = fcntl_getfl(&stdout).map_err(|_| CredentialKeyError::BackupExclusionFailed)?;
        fcntl_setfl(&stdout, flags | OFlags::NONBLOCK)
            .map_err(|_| CredentialKeyError::BackupExclusionFailed)?;
        let mut output = Vec::new();
        let mut ended = None;
        loop {
            if start.elapsed() >= limit {
                #[cfg(test)]
                {
                    observation.timed_out = true;
                }
                return Ok(TmutilOutput {
                    success: false,
                    stdout: output,
                    timed_out: true,
                });
            }
            let mut buffer = [0; 4096];
            let idle = match stdout.read(&mut buffer) {
                Ok(0) => true,
                Ok(length) => {
                    if output.len() + length > TMUTIL_OUTPUT_CAP {
                        #[cfg(test)]
                        {
                            observation.output_capped = true;
                        }
                        return Err(CredentialKeyError::BackupExclusionFailed);
                    }
                    output.extend_from_slice(&buffer[..length]);
                    #[cfg(test)]
                    {
                        observation.output_bytes = output.len();
                    }
                    false
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => true,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(CredentialKeyError::BackupExclusionFailed),
            };
            if ended.is_none() {
                ended = child
                    .try_wait()
                    .map_err(|_| CredentialKeyError::BackupExclusionFailed)?;
            }
            if let Some(status) = ended {
                if idle {
                    return Ok(TmutilOutput {
                        success: status.success(),
                        stdout: output,
                        timed_out: false,
                    });
                }
            } else if idle {
                std::thread::sleep(
                    Duration::from_millis(10).min(limit.saturating_sub(start.elapsed())),
                );
            }
        }
    })();
    // Reap every child. On timeout/read/cap failure, terminate it first.
    let must_stop = match &result {
        Err(_) => true,
        Ok(output) => output.timed_out,
    };
    if must_stop {
        #[cfg(test)]
        {
            observation.kill_attempted = true;
        }
        let kill_succeeded = child.kill().is_ok();
        #[cfg(test)]
        {
            observation.kill_succeeded = kill_succeeded;
        }
        #[cfg(not(test))]
        let _ = kill_succeeded;
    }
    let waited = child.wait();
    let reap_failed = waited.is_err();
    #[cfg(test)]
    {
        observation.waited = waited.is_ok();
        observation.try_wait_exited = matches!(child.try_wait(), Ok(Some(_)));
        if let Ok(status) = waited {
            observation.exit_code = status.code();
            observation.exit_signal = status.signal();
        }
        TMUTIL_OBSERVATIONS.with(|state| state.borrow_mut().push(observation));
    }
    #[cfg(not(test))]
    let _ = waited;
    if reap_failed {
        Err(CredentialKeyError::BackupExclusionFailed)
    } else {
        result
    }
}

#[cfg(test)]
#[derive(Clone)]
struct TmutilTestSettings {
    program: PathBuf,
    limit: Duration,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct TmutilObservation {
    pub program: PathBuf,
    pub operation: String,
    pub pid: u32,
    pub timed_out: bool,
    pub output_bytes: usize,
    pub output_capped: bool,
    pub kill_attempted: bool,
    pub kill_succeeded: bool,
    pub waited: bool,
    pub try_wait_exited: bool,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
}

#[cfg(test)]
thread_local! {
    static TMUTIL_TEST_SETTINGS: std::cell::RefCell<Option<TmutilTestSettings>> = const { std::cell::RefCell::new(None) };
    static TMUTIL_OBSERVATIONS: std::cell::RefCell<Vec<TmutilObservation>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(super) fn with_tmutil_test<T>(
    program: &Path,
    limit: Duration,
    action: impl FnOnce() -> T,
) -> T {
    let temporary_root = std::env::temp_dir()
        .canonicalize()
        .expect("system temporary root");
    let program = program.canonicalize().expect("temporary fake executable");
    assert!(program.starts_with(&temporary_root) && program.is_file());
    assert_ne!(program, Path::new(TMUTIL_EXECUTABLE));
    assert!(!limit.is_zero() && limit <= TMUTIL_RUN_LIMIT);
    struct Reset(Option<TmutilTestSettings>);
    impl Drop for Reset {
        fn drop(&mut self) {
            TMUTIL_TEST_SETTINGS.with(|state| *state.borrow_mut() = self.0.take());
        }
    }
    let reset = Reset(
        TMUTIL_TEST_SETTINGS
            .with(|state| state.replace(Some(TmutilTestSettings { program, limit }))),
    );
    TMUTIL_OBSERVATIONS.with(|state| state.borrow_mut().clear());
    let result = action();
    drop(reset);
    result
}

#[cfg(test)]
pub(super) fn take_tmutil_observations() -> Vec<TmutilObservation> {
    TMUTIL_OBSERVATIONS.with(|state| std::mem::take(&mut *state.borrow_mut()))
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct KeyFaults {
    pub entropy: Option<Vec<Vec<u8>>>,
    pub entropy_fail: bool,
    pub file_barrier_fail: bool,
    pub directory_barrier_fail: bool,
    pub owner: Option<u32>,
    pub write_errno: Option<i32>,
    pub rename_errno: Option<i32>,
    pub cleanup_remove_fail: bool,
    #[cfg(target_os = "macos")]
    pub acl_clear_fail: bool,
}
#[cfg(test)]
thread_local! {
    static KEY_FAULTS: std::cell::RefCell<KeyFaults> = std::cell::RefCell::new(KeyFaults::default());
    static KEY_EVENTS: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
}
#[cfg(test)]
pub(super) fn with_key_faults<T>(faults: KeyFaults, action: impl FnOnce() -> T) -> T {
    struct Reset(Option<KeyFaults>);
    impl Drop for Reset {
        fn drop(&mut self) {
            KEY_FAULTS.with(|state| *state.borrow_mut() = self.0.take().unwrap());
        }
    }
    let reset = Reset(Some(KEY_FAULTS.with(|state| state.replace(faults))));
    KEY_EVENTS.with(|state| state.borrow_mut().clear());
    let result = action();
    drop(reset);
    result
}
#[cfg(test)]
pub(super) fn take_key_events() -> Vec<&'static str> {
    KEY_EVENTS.with(|state| std::mem::take(&mut *state.borrow_mut()))
}

fn event(name: &'static str) {
    #[cfg(test)]
    KEY_EVENTS.with(|state| state.borrow_mut().push(name));
    #[cfg(not(test))]
    let _ = name;
}

#[cfg(test)]
#[path = "credential_keys/tests.rs"]
mod g24_m2_k;
