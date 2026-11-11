// SPDX-License-Identifier: AGPL-3.0-only

//! Admission of encrypted-backup destinations and archive files (M06-T02).
//!
//! Every generation carries an authenticated manifest (inside the archive,
//! under the password-derived key), so what a destination holds is trusted
//! only once it authenticates. POSIX ownership and permission checks add a
//! second line on volumes that honour them: a local volume whose ownership is
//! enforced. Network shares, removable media and volumes mounted with
//! ownership ignored report owners and modes that mean nothing (an exFAT stick
//! shows every file as the current user's, mode 0777), so there the Hub relies
//! on the authenticated encryption alone and the POSIX checks are skipped.

use std::{fs, io, os::unix::fs::MetadataExt, path::Path};

use rustix::process::getuid;

use super::{DataRecoveryError, invalid};

/// What a destination volume's file metadata can be trusted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumeTrust {
    /// A local volume that enforces ownership: POSIX admission applies.
    Posix,
    /// A network, removable or ownership-ignoring volume: only the
    /// authenticated encryption is relied on.
    EncryptionOnly,
}

/// Group and other write bits: another local user could replace an entry.
const FOREIGN_WRITE_BITS: u32 = 0o022;

/// Classify the volume that holds `path`.
pub(crate) fn classify(path: &Path) -> Result<VolumeTrust, DataRecoveryError> {
    #[cfg(test)]
    if let Some(forced) = test_seam::forced() {
        return Ok(forced);
    }
    platform_trust(path).map_err(|source| DataRecoveryError::DestinationUnavailable {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(target_os = "macos")]
fn platform_trust(path: &Path) -> io::Result<VolumeTrust> {
    // <sys/mount.h> mount flags as statfs(2) reports them.
    const MNT_REMOVABLE: u32 = 0x0000_0200;
    const MNT_LOCAL: u32 = 0x0000_1000;
    const MNT_IGNORE_OWNERSHIP: u32 = 0x0020_0000;
    // File systems whose owners and modes are synthesised, not stored.
    const SYNTHETIC_PERMISSIONS: [&str; 5] = ["msdos", "exfat", "ntfs", "cd9660", "udf"];
    let stats = rustix::fs::statfs(path)?;
    let type_name: String = stats
        .f_fstypename
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| char::from(*byte as u8))
        .collect();
    let flags = stats.f_flags;
    if flags & MNT_LOCAL == 0
        || flags & (MNT_REMOVABLE | MNT_IGNORE_OWNERSHIP) != 0
        || SYNTHETIC_PERMISSIONS.contains(&type_name.as_str())
    {
        Ok(VolumeTrust::EncryptionOnly)
    } else {
        Ok(VolumeTrust::Posix)
    }
}

#[cfg(target_os = "linux")]
fn platform_trust(path: &Path) -> io::Result<VolumeTrust> {
    // statfs(2) magic numbers of network file systems and of file systems
    // without stored POSIX ownership.
    const ENCRYPTION_ONLY: [u64; 12] = [
        0x6969,      // NFS
        0x517B,      // SMB
        0xFF53_4D42, // CIFS
        0xFE53_4D42, // SMB2
        0x0102_1997, // 9P
        0x00C3_6400, // Ceph
        0x5346_414F, // AFS
        0x6573_5546, // FUSE (sshfs, rclone and similar)
        0x4D44,      // FAT
        0x2011_BAB0, // exFAT
        0x5346_544E, // NTFS
        0x9660,      // ISO 9660
    ];
    let stats = rustix::fs::statfs(path)?;
    #[allow(clippy::unnecessary_cast, clippy::cast_sign_loss)]
    let magic = (stats.f_type as u64) & 0xFFFF_FFFF;
    Ok(if ENCRYPTION_ONLY.contains(&magic) {
        VolumeTrust::EncryptionOnly
    } else {
        VolumeTrust::Posix
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_trust(_path: &Path) -> io::Result<VolumeTrust> {
    Ok(VolumeTrust::Posix)
}

/// Admit a destination folder before anything is written to it. On a POSIX
/// volume it must be a real directory owned by the Hub's user and writable by
/// no one else, so no other local account can plant or swap a generation.
pub(crate) fn admit_destination(destination: &Path) -> Result<VolumeTrust, DataRecoveryError> {
    let trust = classify(destination)?;
    if trust == VolumeTrust::Posix {
        let metadata = fs::symlink_metadata(destination).map_err(|source| {
            DataRecoveryError::DestinationUnavailable {
                path: destination.to_path_buf(),
                source,
            }
        })?;
        if !metadata.file_type().is_dir() {
            return Err(invalid("the backup destination must be a real directory"));
        }
        if metadata.uid() != getuid().as_raw() || metadata.mode() & FOREIGN_WRITE_BITS != 0 {
            return Err(invalid(
                "the backup destination must be owned by the Hub's user and not writable by \
                 other accounts",
            ));
        }
    }
    Ok(trust)
}

/// Admit an opened archive (or partial) file. On a POSIX volume it must be
/// owned by the Hub's user, writable by no one else and have one link; every
/// volume additionally requires the archive to authenticate before it is
/// trusted, rotated or restored.
pub(crate) fn admit_archive_file(
    metadata: &fs::Metadata,
    trust: VolumeTrust,
) -> Result<(), DataRecoveryError> {
    if !metadata.file_type().is_file() {
        return Err(invalid("a backup archive must be a regular file"));
    }
    if trust == VolumeTrust::Posix
        && (metadata.uid() != getuid().as_raw()
            || metadata.mode() & FOREIGN_WRITE_BITS != 0
            || metadata.nlink() != 1)
    {
        return Err(invalid(
            "a backup archive on a local volume must be owned by the Hub's user, not writable \
             by other accounts and have a single link",
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_seam {
    use std::cell::Cell;

    use super::VolumeTrust;

    thread_local! {
        static FORCED: Cell<Option<VolumeTrust>> = const { Cell::new(None) };
    }

    /// Classify every volume as `trust` on this thread until reset.
    pub(crate) fn force(trust: Option<VolumeTrust>) {
        FORCED.with(|slot| slot.set(trust));
    }

    pub(super) fn forced() -> Option<VolumeTrust> {
        FORCED.with(Cell::get)
    }
}
