// SPDX-License-Identifier: AGPL-3.0-only

//! Admission of encrypted-backup destinations and archive files (M06-T02).
//!
//! Every generation carries an authenticated manifest (inside the archive,
//! under the password-derived key), so what a destination holds is trusted
//! only once it authenticates. POSIX ownership, permission and ACL checks add
//! a second line wherever the volume enforces ownership, removable or not: an
//! APFS or HFS+ disk with ownership enabled enforces owners and modes whether
//! it is internal or plugged in. Network shares and volumes whose ownership is
//! ignored or synthesised (`noowners`, exFAT, FAT, NTFS: every file shows as
//! the current user's) report owners and modes that mean nothing; there the
//! Hub relies on the authenticated encryption alone, skips the POSIX checks
//! and never deletes anything (see `encrypted_backup::retention`).
//!
//! On macOS an extended ACL can let another account write a file, or add and
//! delete entries in a directory, that its mode bits show as private. The ACL
//! is read for the object the Hub holds open (the descriptor's current path,
//! re-checked against the descriptor afterwards), and an `allow` entry that
//! grants any write, add or delete right refuses the object.

use std::{
    fs::{self, File},
    io,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
};

use rustix::{
    fs::{Mode, OFlags, StatFs, fstatfs, open},
    process::getuid,
};

use super::{DataRecoveryError, invalid};

/// What a destination volume's file metadata can be trusted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumeTrust {
    /// A local volume that enforces ownership: POSIX admission applies, and
    /// a protected namespace for removals can be made.
    Posix,
    /// A network or ownership-ignoring volume: only the authenticated
    /// encryption is relied on, and nothing is ever deleted there.
    EncryptionOnly,
}

/// Group and other write bits: another local user could replace an entry.
const FOREIGN_WRITE_BITS: u32 = 0o022;
/// Any group or other access: a protected namespace is the owner's alone.
const FOREIGN_ACCESS_BITS: u32 = 0o077;

/// A destination folder admitted for a run, held open for the whole run.
pub(crate) struct AdmittedDirectory {
    pub(crate) fd: OwnedFd,
    pub(crate) path: PathBuf,
    pub(crate) trust: VolumeTrust,
}

/// Classify the volume that holds `path`.
pub(crate) fn classify(path: &Path) -> Result<VolumeTrust, DataRecoveryError> {
    #[cfg(test)]
    if let Some(forced) = test_seam::forced() {
        return Ok(forced);
    }
    rustix::fs::statfs(path)
        .map(|stats| platform_trust(&stats))
        .map_err(|source| DataRecoveryError::DestinationUnavailable {
            path: path.to_path_buf(),
            source: source.into(),
        })
}

fn classify_held(fd: BorrowedFd<'_>, path: &Path) -> Result<VolumeTrust, DataRecoveryError> {
    #[cfg(test)]
    if let Some(forced) = test_seam::forced() {
        return Ok(forced);
    }
    fstatfs(fd)
        .map(|stats| platform_trust(&stats))
        .map_err(|source| DataRecoveryError::DestinationUnavailable {
            path: path.to_path_buf(),
            source: source.into(),
        })
}

#[cfg(target_os = "macos")]
fn platform_trust(stats: &StatFs) -> VolumeTrust {
    let type_name: String = stats
        .f_fstypename
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| char::from(*byte as u8))
        .collect();
    trust_for_mount(stats.f_flags, &type_name)
}

/// The trust a macOS mount's `statfs(2)` flags and type allow. Removability
/// is not a criterion: removable APFS or HFS+ media with ownership enabled
/// enforce owners and permissions like an internal disk.
#[cfg(target_os = "macos")]
pub(crate) fn trust_for_mount(flags: u32, type_name: &str) -> VolumeTrust {
    // <sys/mount.h> mount flags as statfs(2) reports them.
    const MNT_LOCAL: u32 = 0x0000_1000;
    const MNT_IGNORE_OWNERSHIP: u32 = 0x0020_0000;
    // File systems whose owners and modes are synthesised, not stored.
    const SYNTHETIC_PERMISSIONS: [&str; 5] = ["msdos", "exfat", "ntfs", "cd9660", "udf"];
    if flags & MNT_LOCAL == 0
        || flags & MNT_IGNORE_OWNERSHIP != 0
        || SYNTHETIC_PERMISSIONS.contains(&type_name)
    {
        VolumeTrust::EncryptionOnly
    } else {
        VolumeTrust::Posix
    }
}

#[cfg(target_os = "linux")]
fn platform_trust(stats: &StatFs) -> VolumeTrust {
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
    #[allow(clippy::unnecessary_cast, clippy::cast_sign_loss)]
    let magic = (stats.f_type as u64) & 0xFFFF_FFFF;
    if ENCRYPTION_ONLY.contains(&magic) {
        VolumeTrust::EncryptionOnly
    } else {
        VolumeTrust::Posix
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_trust(_stats: &StatFs) -> VolumeTrust {
    VolumeTrust::Posix
}

/// Admit a destination folder before anything is written to it, and hold it
/// open: every later operation of the run is relative to this descriptor. On
/// a POSIX volume it must be a real directory owned by the Hub's user that no
/// other account can write to, by mode or by ACL, so no other local account
/// can plant, swap or delete a generation.
pub(crate) fn admit_destination(destination: &Path) -> Result<AdmittedDirectory, DataRecoveryError> {
    let unavailable = |source: io::Error| DataRecoveryError::DestinationUnavailable {
        path: destination.to_path_buf(),
        source,
    };
    let fd = open(
        destination,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| unavailable(error.into()))?;
    let trust = classify_held(fd.as_fd(), destination)?;
    if trust == VolumeTrust::Posix {
        admit_held_directory(&fd, destination, FOREIGN_WRITE_BITS).map_err(|error| match error {
            DataRecoveryError::InvalidBackup(_) => invalid(
                "the backup destination must be owned by the Hub's user and not writable by \
                 other accounts, by its mode or by an access control list",
            ),
            other => other,
        })?;
    }
    Ok(AdmittedDirectory {
        fd,
        path: destination.to_path_buf(),
        trust,
    })
}

/// Admit a directory the Hub made for its own removals: owned by the Hub's
/// user, no access for anyone else by mode or ACL.
pub(crate) fn admit_protected_directory(fd: &OwnedFd, path: &Path) -> Result<(), DataRecoveryError> {
    admit_held_directory(fd, path, FOREIGN_ACCESS_BITS)
}

fn admit_held_directory(
    fd: &OwnedFd,
    path: &Path,
    forbidden_bits: u32,
) -> Result<(), DataRecoveryError> {
    let metadata = File::from(
        fd.try_clone()
            .map_err(|source| DataRecoveryError::DestinationUnavailable {
                path: path.to_path_buf(),
                source,
            })?,
    )
    .metadata()
    .map_err(|source| DataRecoveryError::DestinationUnavailable {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != getuid().as_raw()
        || metadata.mode() & forbidden_bits != 0
    {
        return Err(invalid(format!(
            "{} must be a directory owned by the Hub's user and not writable by other accounts",
            path.display()
        )));
    }
    refuse_foreign_write_acl(fd.as_fd(), &metadata, path)
}

/// Admit an opened archive (or partial) file and return its metadata. On a
/// POSIX volume it must be owned by the Hub's user, writable by no one else
/// (by mode or by ACL) and have one link; every volume additionally requires
/// the archive to authenticate before it is trusted, rotated or restored.
pub(crate) fn admit_archive_file(
    file: &File,
    path: &Path,
    trust: VolumeTrust,
) -> Result<fs::Metadata, DataRecoveryError> {
    let metadata = file
        .metadata()
        .map_err(|source| super::io_error("inspecting backup archive", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(invalid("a backup archive must be a regular file"));
    }
    if trust == VolumeTrust::Posix {
        if metadata.uid() != getuid().as_raw()
            || metadata.mode() & FOREIGN_WRITE_BITS != 0
            || metadata.nlink() != 1
        {
            return Err(invalid(
                "a backup archive on a local volume must be owned by the Hub's user, not \
                 writable by other accounts and have a single link",
            ));
        }
        refuse_foreign_write_acl(file.as_fd(), &metadata, path)?;
    }
    Ok(metadata)
}

/// The rights an `allow` ACL entry must not grant on an admitted object: any
/// that writes, appends, adds or deletes an entry, or changes the object's
/// attributes, security or owner.
#[cfg(any(target_os = "macos", test))]
const UNSAFE_ACL_RIGHTS: [&str; 10] = [
    "write",
    "append",
    "add_file",
    "add_subdirectory",
    "delete",
    "delete_child",
    "writeattr",
    "writeextattr",
    "writesecurity",
    "chown",
];

/// Whether an `ls -le` listing holds an `allow` entry with an unsafe right.
/// The first line is the object itself; each ACL entry follows on its own
/// line, numbered `0:`, `1:`..., ending in `allow <rights>` or `deny <rights>`
/// (an inherited entry says `inherited` before it).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn acl_listing_grants_write(listing: &str) -> bool {
    listing.lines().skip(1).any(|line| {
        let line = line.trim_start();
        let digits = line.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || !line[digits..].starts_with(':') {
            return false;
        }
        let tokens: Vec<&str> = line[digits + 1..].split_whitespace().collect();
        let Some(position) = tokens.iter().rposition(|token| *token == "allow") else {
            return false;
        };
        tokens[position + 1..]
            .iter()
            .flat_map(|rights| rights.split(','))
            .any(|right| UNSAFE_ACL_RIGHTS.contains(&right.trim()))
    })
}

/// Refuse the held object when its ACL lets anyone write, add or delete.
/// macOS has no safe descriptor-based ACL call and the crate forbids unsafe
/// code, so `ls -lde` lists the descriptor's current path (`F_GETPATH`); the
/// path must still name the held object afterwards, otherwise the listing is
/// not trusted and the object is refused.
#[cfg(target_os = "macos")]
fn refuse_foreign_write_acl(
    fd: BorrowedFd<'_>,
    held: &fs::Metadata,
    path: &Path,
) -> Result<(), DataRecoveryError> {
    use std::{
        ffi::OsStr,
        os::unix::ffi::OsStrExt,
        process::{Command, Stdio},
    };

    let refuse = || {
        invalid(format!(
            "an access control list on {} lets another account write to it, or its access \
             control list could not be read",
            path.display()
        ))
    };
    let current = rustix::fs::getpath(fd).map_err(|_| refuse())?;
    let current = Path::new(OsStr::from_bytes(current.to_bytes()));
    let output = Command::new("/bin/ls")
        .arg("-lde")
        .arg(current)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| refuse())?;
    let listed = fs::symlink_metadata(current).map_err(|_| refuse())?;
    if !output.status.success() || listed.dev() != held.dev() || listed.ino() != held.ino() {
        return Err(refuse());
    }
    if acl_listing_grants_write(&String::from_utf8_lossy(&output.stdout)) {
        return Err(refuse());
    }
    Ok(())
}

/// Elsewhere a POSIX ACL's mask is the group bits, which the mode check
/// already requires to be free of write access.
#[cfg(not(target_os = "macos"))]
#[allow(clippy::unnecessary_wraps)]
fn refuse_foreign_write_acl(
    _fd: BorrowedFd<'_>,
    _held: &fs::Metadata,
    _path: &Path,
) -> Result<(), DataRecoveryError> {
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
