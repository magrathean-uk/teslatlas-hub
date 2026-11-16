// SPDX-License-Identifier: AGPL-3.0-only

//! Fleet command identity, separate from the Hub's LAN TLS identity.

use std::{
    fmt,
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

use crate::platform::private_directory::{self, EntryState, PrivateDirectory};
use zeroize::Zeroizing;

const KEY_NAME: &str = "fleet-command-key.pem";
const LOCK_NAME: &str = "fleet-command-key.lock";
const MAX_KEY_BYTES: u64 = 4096;

/// Public export only. The private key never leaves `data_dir/secrets`.
pub struct CommandPublicKey(String);

impl CommandPublicKey {
    pub fn public_pem(&self) -> &str {
        &self.0
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
    #[error("fleet_partner_key_unsafe")]
    Unsafe,
    #[error("fleet_partner_key_invalid")]
    Invalid,
    #[error("fleet_partner_key_busy")]
    Busy,
    #[error("fleet_partner_key_generation")]
    Generation,
}

/// Create once, or export the existing P-256 identity. Never silently rotate
/// a malformed, inaccessible or unsafe key. The existing backup exclusion
/// covers `fleet-command-key.pem`; no private material enters a report.
pub fn ensure_command_key(data_dir: &Path) -> Result<CommandPublicKey, FleetKeyError> {
    let directory = private_directory::open_validated(&data_dir.join("secrets"), true)
        .map_err(|_| FleetKeyError::Storage)?;
    let lock = match directory
        .inspect_entry(LOCK_NAME)
        .map_err(|_| FleetKeyError::Storage)?
    {
        EntryState::Absent => directory
            .create_exclusive(LOCK_NAME)
            .map_err(|_| FleetKeyError::Storage)?,
        EntryState::Regular(fd) => File::from(fd),
        EntryState::Unsafe(_) => return Err(FleetKeyError::Unsafe),
    };
    validate_file(&lock)?;
    if !private_directory::try_lock_exclusive(&lock).map_err(|_| FleetKeyError::Storage)? {
        return Err(FleetKeyError::Busy);
    }
    match directory
        .inspect_entry(KEY_NAME)
        .map_err(|_| FleetKeyError::Storage)?
    {
        EntryState::Regular(fd) => read_key(File::from(fd)),
        EntryState::Unsafe(_) => Err(FleetKeyError::Unsafe),
        EntryState::Absent => create_key(&directory),
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

fn read_key(file: File) -> Result<CommandPublicKey, FleetKeyError> {
    validate_file(&file)?;
    let mut pem = Zeroizing::new(String::new());
    file.take(MAX_KEY_BYTES + 1)
        .read_to_string(&mut pem)
        .map_err(|_| FleetKeyError::Invalid)?;
    if pem.len() as u64 > MAX_KEY_BYTES {
        return Err(FleetKeyError::Invalid);
    }
    let key = rcgen::KeyPair::from_pem(&pem).map_err(|_| FleetKeyError::Invalid)?;
    if key.algorithm() != &rcgen::PKCS_ECDSA_P256_SHA256 {
        return Err(FleetKeyError::Invalid);
    }
    Ok(CommandPublicKey(key.public_key_pem()))
}

fn create_key(directory: &PrivateDirectory) -> Result<CommandPublicKey, FleetKeyError> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .map_err(|_| FleetKeyError::Generation)?;
    let pem = Zeroizing::new(key.serialize_pem());
    let mut file = directory
        .create_exclusive(KEY_NAME)
        .map_err(|_| FleetKeyError::Storage)?;
    let result = (|| {
        file.write_all(pem.as_bytes())
            .map_err(|_| FleetKeyError::Storage)?;
        private_directory::durable_sync_fd(std::os::fd::AsFd::as_fd(&file))
            .map_err(|_| FleetKeyError::Storage)?;
        directory
            .durable_sync()
            .map_err(|_| FleetKeyError::Storage)?;
        Ok(CommandPublicKey(key.public_key_pem()))
    })();
    if result.is_err() {
        let _ = directory.remove(KEY_NAME);
        let _ = directory.durable_sync();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_partner_key_is_p256_private_and_reused() {
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
        assert!(
            crate::storage::data_recovery::CAR_CONTROL_SECRET_FILES
                .contains(&"fleet-command-key.pem")
        );
    }

    #[test]
    fn fleet_partner_key_refuses_invalid_material_without_replacing_it() {
        for contents in [
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
    fn fleet_partner_key_refuses_symlinks_directories_and_hardlinks() {
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
    fn fleet_partner_key_refuses_wide_permissions_and_busy_writer() {
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
    fn fleet_partner_key_refuses_unavailable_storage_and_directory_symlink() {
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
}
