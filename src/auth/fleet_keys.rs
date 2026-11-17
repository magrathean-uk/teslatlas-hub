// SPDX-License-Identifier: AGPL-3.0-only

//! Fleet command identity, separate from the Hub's LAN TLS identity.
//!
//! Library-owned PEM and DER buffers are bounded and wiped. rcgen keeps an
//! internal DER vector and signing key without a wiping API; that dependency
//! residue cannot be erased here. Avoid its PEM parser/serializer, which would
//! introduce additional unwiped DER and base64 allocations.

use std::{
    fmt,
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

#[cfg(not(test))]
use crate::auth::credential_keys::SystemBackupExclusion;
use crate::auth::credential_keys::{BackupExclusion, ExclusionState};
use crate::platform::private_directory::{self, EntryState, PrivateDirectory};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use zeroize::Zeroizing;

const PRIVATE_KEY_BEGIN: &str = concat!("-----BEGIN ", "PRIVATE KEY-----");
const PRIVATE_KEY_END: &str = concat!("-----END ", "PRIVATE KEY-----");

const KEY_NAME: &str = "fleet-command-key.pem";
const LOCK_NAME: &str = "fleet-command-key.lock";
const MAX_KEY_BYTES: u64 = 4096;

/// Public export only. The private key never leaves `data_dir/secrets`.
pub struct CommandPublicKey {
    pem: String,
    /// The uncompressed P-256 point, as Tesla reports a registered key.
    point: Vec<u8>,
}

impl CommandPublicKey {
    fn of(key: &rcgen::KeyPair) -> Self {
        Self {
            pem: key.public_key_pem(),
            point: key.public_key_raw().to_vec(),
        }
    }

    pub fn public_pem(&self) -> &str {
        &self.pem
    }

    /// The public point as lowercase hex, the form Tesla's partner
    /// `public_key` answer uses.
    pub fn public_point_hex(&self) -> String {
        self.point
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn synthetic_for_test(point: Vec<u8>) -> Self {
        Self {
            pem: String::new(),
            point,
        }
    }
}

impl fmt::Debug for CommandPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CommandPublicKey([redacted])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FleetKeyError {
    #[error("fleet_partner_key_storage")]
    Storage,
    #[error("fleet_partner_key_backup_exclusion")]
    BackupExclusion,
    #[error("fleet_partner_key_unsafe")]
    Unsafe,
    #[error("fleet_partner_key_invalid")]
    Invalid,
    #[error("fleet_partner_key_busy")]
    Busy,
    #[error("fleet_partner_key_generation")]
    Generation,
}

/// Create once, or export the existing P-256 identity after verifying backup
/// exclusion and durability. An empty or torn legacy file is refused with
/// `fleet_partner_key_invalid`, left unchanged, and never silently rotated.
pub fn ensure_command_key(data_dir: &Path) -> Result<CommandPublicKey, FleetKeyError> {
    #[cfg(not(test))]
    let exclusion = SystemBackupExclusion;
    // Unit tests cannot run platform tools. Explicit refusal and ordering tests
    // exercise the same barrier through the injected adapter below.
    #[cfg(test)]
    let exclusion = TestExclusion;
    ensure_command_key_with(data_dir, &exclusion)
}

pub(crate) fn ensure_command_key_with(
    data_dir: &Path,
    exclusion: &dyn BackupExclusion,
) -> Result<CommandPublicKey, FleetKeyError> {
    let parent =
        private_directory::open_validated(data_dir, false).map_err(|_| FleetKeyError::Storage)?;
    let directory = private_directory::open_validated(&data_dir.join("secrets"), true)
        .map_err(|_| FleetKeyError::Storage)?;
    sync_directory(&parent)?;
    // Identical barrier to credential_keys::ensure_backup: verify even when
    // secrets/ is new and no credential vault has ever opened it for writing.
    let state = exclusion
        .ensure_excluded(directory.path())
        .map_err(|_| FleetKeyError::BackupExclusion)?;
    if cfg!(target_os = "macos") && state != ExclusionState::Verified {
        return Err(FleetKeyError::BackupExclusion);
    }
    let lock = match directory
        .inspect_entry(LOCK_NAME)
        .map_err(|_| FleetKeyError::Storage)?
    {
        EntryState::Absent => directory.create_exclusive(LOCK_NAME).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                FleetKeyError::Busy
            } else {
                FleetKeyError::Storage
            }
        })?,
        EntryState::Regular(fd) => File::from(fd),
        EntryState::Unsafe(_) => return Err(FleetKeyError::Unsafe),
    };
    validate_file(&lock)?;
    if !private_directory::try_lock_exclusive(&lock).map_err(|_| FleetKeyError::Storage)? {
        return Err(FleetKeyError::Busy);
    }
    // Only our UUID-named abandoned staging files are removed, under the lock.
    for name in directory
        .entry_names()
        .map_err(|_| FleetKeyError::Storage)?
    {
        if name
            .strip_prefix("fleet-command-key-")
            .and_then(|name| name.strip_suffix(".tmp"))
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        {
            match directory
                .inspect_entry(&name)
                .map_err(|_| FleetKeyError::Storage)?
            {
                EntryState::Regular(fd) => validate_file(&File::from(fd))?,
                EntryState::Absent => continue,
                EntryState::Unsafe(_) => return Err(FleetKeyError::Unsafe),
            }
            directory
                .remove(&name)
                .map_err(|_| FleetKeyError::Storage)?;
        }
    }
    match directory
        .inspect_entry(KEY_NAME)
        .map_err(|_| FleetKeyError::Storage)?
    {
        EntryState::Regular(fd) => {
            let file = File::from(fd);
            let public = read_key(&file)?;
            private_directory::durable_sync_fd(std::os::fd::AsFd::as_fd(&file))
                .map_err(|_| FleetKeyError::Storage)?;
            sync_directory(&directory)?;
            Ok(public)
        }
        EntryState::Unsafe(_) => Err(FleetKeyError::Unsafe),
        EntryState::Absent => create_key(&directory),
    }
}

fn sync_directory(directory: &PrivateDirectory) -> Result<(), FleetKeyError> {
    #[cfg(test)]
    if FAIL_DIRECTORY_SYNC.with(|fail| fail.get()) {
        return Err(FleetKeyError::Storage);
    }
    directory.durable_sync().map_err(|_| FleetKeyError::Storage)
}

#[cfg(test)]
thread_local! {
    static FAIL_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
struct TestExclusion;
#[cfg(test)]
impl BackupExclusion for TestExclusion {
    fn ensure_excluded(
        &self,
        _: &Path,
    ) -> Result<ExclusionState, crate::auth::credential_keys::CredentialKeyError> {
        Ok(ExclusionState::Verified)
    }
}

fn validate_file(file: &File) -> Result<(), FleetKeyError> {
    let m = file.metadata().map_err(|_| FleetKeyError::Storage)?;
    if !m.is_file()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.nlink() != 1
        || m.mode() & 0o7777 != 0o600
    {
        return Err(FleetKeyError::Unsafe);
    }
    Ok(())
}

fn read_key(file: &File) -> Result<CommandPublicKey, FleetKeyError> {
    validate_file(file)?;
    let mut pem = Zeroizing::new(String::with_capacity(MAX_KEY_BYTES as usize + 1));
    file.take(MAX_KEY_BYTES + 1)
        .read_to_string(&mut pem)
        .map_err(|_| FleetKeyError::Invalid)?;
    if pem.len() as u64 > MAX_KEY_BYTES {
        return Err(FleetKeyError::Invalid);
    }
    let encoded = pem
        .trim()
        .strip_prefix(PRIVATE_KEY_BEGIN)
        .and_then(|s| s.strip_suffix(PRIVATE_KEY_END))
        .ok_or(FleetKeyError::Invalid)?;
    let mut base64 = Zeroizing::new(Vec::with_capacity(MAX_KEY_BYTES as usize));
    base64.extend(encoded.bytes().filter(|b| !b.is_ascii_whitespace()));
    let mut der = Zeroizing::new(vec![0u8; MAX_KEY_BYTES as usize]);
    let len = STANDARD
        .decode_slice(&base64, &mut der)
        .map_err(|_| FleetKeyError::Invalid)?;
    der.truncate(len);
    let key = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
        &rustls::pki_types::PrivatePkcs8KeyDer::from(der.as_slice()),
        &rcgen::PKCS_ECDSA_P256_SHA256,
    )
    .map_err(|_| FleetKeyError::Invalid)?;
    Ok(CommandPublicKey::of(&key))
}

fn create_key(directory: &PrivateDirectory) -> Result<CommandPublicKey, FleetKeyError> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .map_err(|_| FleetKeyError::Generation)?;
    let der = Zeroizing::new(key.serialize_der());
    let mut encoded = Zeroizing::new(vec![0u8; MAX_KEY_BYTES as usize]);
    let len = STANDARD
        .encode_slice(&der, &mut encoded)
        .map_err(|_| FleetKeyError::Generation)?;
    let mut pem = Zeroizing::new(Vec::with_capacity(MAX_KEY_BYTES as usize));
    pem.extend_from_slice(PRIVATE_KEY_BEGIN.as_bytes());
    pem.push(b'\n');
    for line in encoded[..len].chunks(64) {
        pem.extend_from_slice(line);
        pem.push(b'\n');
    }
    pem.extend_from_slice(PRIVATE_KEY_END.as_bytes());
    pem.push(b'\n');
    let name = format!("fleet-command-key-{}.tmp", uuid::Uuid::new_v4());
    let mut file = directory
        .create_exclusive(&name)
        .map_err(|_| FleetKeyError::Storage)?;
    let result = (|| {
        file.write_all(&pem).map_err(|_| FleetKeyError::Storage)?;
        private_directory::durable_sync_fd(std::os::fd::AsFd::as_fd(&file))
            .map_err(|_| FleetKeyError::Storage)?;
        directory
            .rename(&name, KEY_NAME)
            .map_err(|_| FleetKeyError::Storage)?;
        sync_directory(directory)?;
        Ok(CommandPublicKey::of(&key))
    })();
    if result.is_err() {
        // A complete renamed file remains recoverable when the final directory
        // barrier fails. Never remove an identity that may already be durable.
        let _ = directory.remove(&name);
        let _ = sync_directory(directory);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_keys_is_p256_private_and_reused() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::private_tempdir().expect("synthetic data directory");
        let first = ensure_command_key(tmp.path()).expect("create test key");
        let second = ensure_command_key(tmp.path()).expect("reuse test key");
        assert!(first.public_pem() == second.public_pem());
        assert!(first.public_pem().starts_with("-----BEGIN PUBLIC KEY-----"));
        let path = tmp.path().join("secrets/fleet-command-key.pem");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let pem = zeroize::Zeroizing::new(std::fs::read_to_string(path).unwrap());
        let key = rcgen::KeyPair::from_pem(&pem).expect("parse test private key");
        assert_eq!(key.algorithm(), &rcgen::PKCS_ECDSA_P256_SHA256);
        assert!(key.public_key_pem() == first.public_pem());
        // The uncompressed point Tesla reports: 0x04, then X and Y.
        let hex = first.public_point_hex();
        assert!(hex.len() == 130 && hex.starts_with("04") && hex == second.public_point_hex());
        assert!(
            hex == key
                .public_key_raw()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        assert!(
            crate::storage::data_recovery::CAR_CONTROL_SECRET_FILES
                .contains(&"fleet-command-key.pem")
        );
    }

    #[test]
    fn fleet_keys_pem_bytes_match_pkcs8_serializer() {
        let tmp = crate::private_tempdir().unwrap();
        ensure_command_key(tmp.path()).unwrap();
        let pem = Zeroizing::new(
            std::fs::read_to_string(tmp.path().join("secrets").join(KEY_NAME)).unwrap(),
        );
        let key = rcgen::KeyPair::from_pem(&pem).unwrap();
        // Independent serializer pins both PKCS#8 markers, LF newlines,
        // base64 wrapping and the trailing newline without source key material.
        let expected = Zeroizing::new(key.serialize_pem());
        assert!(pem.as_bytes() == expected.as_bytes());
    }

    #[test]
    fn fleet_keys_refuses_invalid_material_without_replacing_it() {
        for contents in [
            String::new(),
            format!("{PRIVATE_KEY_BEGIN}\nMIGH"),
            "synthetic-invalid-pem".to_owned(),
            "x".repeat(4097),
            rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384)
                .unwrap()
                .serialize_pem(),
        ] {
            let tmp = crate::private_tempdir().unwrap();
            let dir = private_directory::open_validated(&tmp.path().join("secrets"), true).unwrap();
            dir.create_exclusive(KEY_NAME)
                .unwrap()
                .write_all(contents.as_bytes())
                .unwrap();
            assert!(matches!(
                ensure_command_key(tmp.path()),
                Err(FleetKeyError::Invalid)
            ));
            assert!(
                std::fs::read_to_string(tmp.path().join("secrets").join(KEY_NAME)).unwrap()
                    == contents
            );
        }
    }

    #[test]
    fn fleet_keys_refuses_symlinks_directories_and_hardlinks() {
        use std::os::unix::fs::symlink;
        for kind in 0..4 {
            let tmp = crate::private_tempdir().unwrap();
            let dir = private_directory::open_validated(&tmp.path().join("secrets"), true).unwrap();
            let path = dir.path().join(KEY_NAME);
            let outside = tmp.path().join("synthetic-target");
            std::fs::write(&outside, "synthetic-unchanged").unwrap();
            match kind {
                0 => symlink(&outside, &path).unwrap(),
                1 => std::fs::create_dir(&path).unwrap(),
                2 => std::fs::hard_link(&outside, &path).unwrap(),
                _ => symlink(&outside, dir.path().join(LOCK_NAME)).unwrap(),
            }
            assert!(matches!(
                ensure_command_key(tmp.path()),
                Err(FleetKeyError::Unsafe)
            ));
            assert!(std::fs::read_to_string(&outside).unwrap() == "synthetic-unchanged");
        }
    }

    #[test]
    fn fleet_keys_refuses_wide_permissions_and_busy_writer() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::private_tempdir().unwrap();
        ensure_command_key(tmp.path()).unwrap();
        let path = tmp.path().join("secrets").join(KEY_NAME);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            ensure_command_key(tmp.path()),
            Err(FleetKeyError::Unsafe)
        ));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let lock = File::open(tmp.path().join("secrets").join(LOCK_NAME)).unwrap();
        assert!(private_directory::try_lock_exclusive(&lock).unwrap());
        assert!(matches!(
            ensure_command_key(tmp.path()),
            Err(FleetKeyError::Busy)
        ));
    }

    #[test]
    fn fleet_keys_refuses_unavailable_storage_and_directory_symlink() {
        let tmp = crate::private_tempdir().unwrap();
        let file = tmp.path().join("synthetic-file");
        std::fs::write(&file, "synthetic-data").unwrap();
        assert!(matches!(
            ensure_command_key(&file),
            Err(FleetKeyError::Storage)
        ));
        let target = tmp.path().join("synthetic-directory");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, tmp.path().join("secrets")).unwrap();
        assert!(matches!(
            ensure_command_key(tmp.path()),
            Err(FleetKeyError::Storage)
        ));
        assert!(std::fs::read_dir(target).unwrap().next().is_none());
    }

    #[test]
    fn fleet_keys_barriers_are_ordered_before_export_and_on_reuse() {
        use std::{cell::RefCell, rc::Rc};
        let tmp = crate::private_tempdir().unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = events.clone();
        let data = tmp.path().to_owned();
        let _observer = private_directory::observe_for_test(Box::new(move |operation, path| {
            if operation == "directory_barrier" && path == data {
                // A parent sync before mkdir would fail this assertion.
                assert!(data.join("secrets").is_dir());
            }
            observed
                .borrow_mut()
                .push((operation.to_owned(), path.to_owned()));
        }));
        struct OrderedExclusion(Rc<RefCell<Vec<(String, std::path::PathBuf)>>>);
        impl BackupExclusion for OrderedExclusion {
            fn ensure_excluded(
                &self,
                path: &Path,
            ) -> Result<ExclusionState, crate::auth::credential_keys::CredentialKeyError>
            {
                assert!(path.is_dir());
                assert!(!path.join(KEY_NAME).exists());
                self.0
                    .borrow_mut()
                    .push(("exclusion".to_owned(), path.to_owned()));
                Ok(ExclusionState::Verified)
            }
        }
        let exclusion = OrderedExclusion(events.clone());
        ensure_command_key_with(tmp.path(), &exclusion).unwrap();
        let events_ref = events.borrow();
        let parent = events_ref
            .iter()
            .position(|(op, path)| op == "directory_barrier" && path == tmp.path())
            .unwrap();
        let exclusion = events_ref
            .iter()
            .position(|(op, _)| op == "exclusion")
            .unwrap();
        let create = events_ref
            .iter()
            .position(|(op, path)| {
                op == "create" && path.extension().is_some_and(|ext| ext == "tmp")
            })
            .unwrap();
        let file = events_ref
            .iter()
            .position(|(op, _)| op == "file_barrier")
            .unwrap();
        let rename = events_ref
            .iter()
            .position(|(op, _)| op == "rename")
            .unwrap();
        let final_sync = events_ref
            .iter()
            .rposition(|(op, path)| {
                op == "directory_barrier" && path == &tmp.path().join("secrets")
            })
            .unwrap();
        assert!(
            parent < exclusion
                && exclusion < create
                && create < file
                && file < rename
                && rename < final_sync
        );
        drop(events_ref);
        events.borrow_mut().clear();
        ensure_command_key(tmp.path()).unwrap();
        let events = events.borrow();
        let file = events
            .iter()
            .position(|(op, _)| op == "file_barrier")
            .unwrap();
        let final_sync = events
            .iter()
            .rposition(|(op, path)| {
                op == "directory_barrier" && path == &tmp.path().join("secrets")
            })
            .unwrap();
        assert!(file < final_sync);
        assert!(
            !events
                .iter()
                .any(|(op, _)| op == "create" || op == "rename")
        );
    }

    #[test]
    fn fleet_keys_refusing_exclusion_never_publishes_or_exports() {
        struct Refuse(bool);
        impl BackupExclusion for Refuse {
            fn ensure_excluded(
                &self,
                path: &Path,
            ) -> Result<ExclusionState, crate::auth::credential_keys::CredentialKeyError>
            {
                assert!(path.is_dir());
                if self.0 {
                    Err(crate::auth::credential_keys::CredentialKeyError::BackupExclusionFailed)
                } else {
                    Ok(ExclusionState::NotApplicable)
                }
            }
        }
        let tmp = crate::private_tempdir().unwrap();
        assert!(matches!(
            ensure_command_key_with(tmp.path(), &Refuse(true)),
            Err(FleetKeyError::BackupExclusion)
        ));
        assert!(!tmp.path().join("secrets").join(KEY_NAME).exists());
        #[cfg(target_os = "macos")]
        assert!(matches!(
            ensure_command_key_with(tmp.path(), &Refuse(false)),
            Err(FleetKeyError::BackupExclusion)
        ));
        ensure_command_key(tmp.path()).unwrap();
        assert!(matches!(
            ensure_command_key_with(tmp.path(), &Refuse(true)),
            Err(FleetKeyError::BackupExclusion)
        ));
    }

    #[test]
    fn fleet_keys_directory_failure_exports_nothing_and_preserves_complete_key() {
        struct FailAfterExclusion;
        impl BackupExclusion for FailAfterExclusion {
            fn ensure_excluded(
                &self,
                _: &Path,
            ) -> Result<ExclusionState, crate::auth::credential_keys::CredentialKeyError>
            {
                FAIL_DIRECTORY_SYNC.with(|fail| fail.set(true));
                Ok(ExclusionState::Verified)
            }
        }
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                FAIL_DIRECTORY_SYNC.with(|fail| fail.set(false));
            }
        }
        for reuse in [false, true] {
            let tmp = crate::private_tempdir().unwrap();
            let before = reuse.then(|| ensure_command_key(tmp.path()).unwrap());
            let reset = Reset;
            assert!(matches!(
                ensure_command_key_with(tmp.path(), &FailAfterExclusion),
                Err(FleetKeyError::Storage)
            ));
            drop(reset);
            let file = File::open(tmp.path().join("secrets").join(KEY_NAME)).unwrap();
            let preserved = read_key(&file).unwrap();
            let recovered = ensure_command_key(tmp.path()).unwrap();
            assert_eq!(preserved.public_pem(), recovered.public_pem());
            if let Some(before) = before {
                assert_eq!(before.public_pem(), recovered.public_pem());
            }
        }
    }

    #[test]
    fn fleet_keys_removes_crash_temporaries_and_never_publishes_before_file_barrier() {
        let tmp = crate::private_tempdir().unwrap();
        let dir = private_directory::open_validated(&tmp.path().join("secrets"), true).unwrap();
        let stale = format!("fleet-command-key-{}.tmp", uuid::Uuid::new_v4());
        dir.create_exclusive(&stale)
            .unwrap()
            .write_all(b"torn")
            .unwrap();
        let (result, _) =
            private_directory::with_sync_faults(true, || ensure_command_key(tmp.path()));
        assert!(matches!(result, Err(FleetKeyError::Storage)));
        assert!(!dir.path().join(KEY_NAME).exists());
        assert!(!dir.path().join(stale).exists());
        assert!(
            !dir.entry_names()
                .unwrap()
                .iter()
                .any(|name| name.ends_with(".tmp"))
        );
        ensure_command_key(tmp.path()).unwrap();
    }

    #[test]
    fn fleet_keys_first_caller_race_is_busy_not_storage() {
        use std::sync::{Arc, Barrier};
        let tmp = crate::private_tempdir().unwrap();
        private_directory::open_validated(&tmp.path().join("secrets"), true).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = tmp.path().to_owned();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let _observer =
                        private_directory::observe_for_test(Box::new(move |op, path| {
                            if op == "create"
                                && path.file_name().is_some_and(|name| name == LOCK_NAME)
                            {
                                barrier.wait(); // Both have observed Absent before either creates.
                            }
                        }));
                    ensure_command_key(&path)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(FleetKeyError::Busy)))
                .count(),
            1
        );
    }

    #[test]
    fn fleet_keys_durability_failure_exports_nothing_and_can_retry() {
        let tmp = crate::private_tempdir().unwrap();
        let (result, _) =
            private_directory::with_sync_faults(true, || ensure_command_key(tmp.path()));
        assert!(matches!(result, Err(FleetKeyError::Storage)));
        assert!(!tmp.path().join("secrets").join(KEY_NAME).exists());
        assert!(ensure_command_key(tmp.path()).is_ok());
    }
}
