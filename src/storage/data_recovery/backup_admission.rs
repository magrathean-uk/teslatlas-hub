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
//! is read for the object the Hub holds open, addressed by its identity on
//! its volume rather than by a pathname (M06-T11), and an `allow` entry that
//! grants any write, add or delete right refuses the object.
//!
//! A destination is opened as named, never through a symbolic link at its
//! end, and every check applies to the directory actually opened (M06-T14):
//! its ownership, mode and ACL by descriptor, and whether it lies inside the
//! live data directory by walking `..` from that descriptor, so a parent
//! folder renamed or replaced around the open cannot redirect the run. The
//! shipped `backup` command asks for an owner-only destination: no access at
//! all for other accounts, by mode or by ACL.

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
    fs::{AtFlags, Mode, OFlags, StatFs, fstatfs, open, stat, statat},
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

/// How private a destination must be on a volume that enforces ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DestinationAccess {
    /// No other account may write to it, by mode or by ACL (the service's
    /// schedule, M06-T02).
    NoForeignWrite,
    /// No other account may have any access, by mode or by ACL (the shipped
    /// `backup` command's private folder, M06-T14).
    OwnerOnly,
}

/// Which ACL `allow` entries refuse an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AclRule {
    /// Entries that grant any write, add or delete right.
    NoWrite,
    /// Every `allow` entry.
    NoAccess,
}

/// The deepest destination whose ancestry is checked; a deeper one is
/// refused.
const MAX_DESTINATION_DEPTH: usize = 256;

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
/// open: every later operation of the run is relative to this descriptor.
///
/// The folder is opened as named, refusing a symbolic link as its last
/// component; its canonical path is kept for reports and run records only,
/// and must name the opened directory. On a POSIX volume it must be a real
/// directory owned by the Hub's user that no other account can write to (or,
/// with [`DestinationAccess::OwnerOnly`], reach at all), by mode or by ACL,
/// so no other local account can plant, swap or delete a generation.
pub(crate) fn admit_destination(
    destination: &Path,
    access: DestinationAccess,
) -> Result<AdmittedDirectory, DataRecoveryError> {
    let unavailable = |source: io::Error| DataRecoveryError::DestinationUnavailable {
        path: destination.to_path_buf(),
        source,
    };
    // Without a trailing `/` or `.`, which would make the open follow a
    // link at the end of the name.
    let named: PathBuf = destination.components().collect();
    let link_refused = || invalid("the backup destination must be a folder, not a symbolic link");
    #[cfg(test)]
    test_seam::destination("before_open");
    let fd = match open(
        &named,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(error) => {
            if fs::symlink_metadata(&named).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
                return Err(link_refused());
            }
            return Err(unavailable(error.into()));
        }
    };
    #[cfg(test)]
    test_seam::destination("after_open");
    let held = File::from(fd.try_clone().map_err(unavailable)?)
        .metadata()
        .map_err(unavailable)?;
    let same = |metadata: &fs::Metadata| metadata.dev() == held.dev() && metadata.ino() == held.ino();
    // The name, and then the canonical path, must still lead to the
    // directory opened; neither is used to reach it again.
    let named_now = fs::symlink_metadata(&named).map_err(unavailable)?;
    if named_now.file_type().is_symlink() {
        return Err(link_refused());
    }
    let canonical = named.canonicalize().map_err(unavailable)?;
    if !same(&named_now) || !fs::metadata(&canonical).is_ok_and(|metadata| same(&metadata)) {
        return Err(invalid(
            "the backup destination changed while it was opened; nothing was written",
        ));
    }
    let trust = classify_held(fd.as_fd(), &canonical)?;
    if trust == VolumeTrust::Posix {
        let (bits, rule, message) = match access {
            DestinationAccess::NoForeignWrite => (
                FOREIGN_WRITE_BITS,
                AclRule::NoWrite,
                "the backup destination must be owned by the Hub's user and not writable by \
                 other accounts, by its mode or by an access control list",
            ),
            DestinationAccess::OwnerOnly => (
                FOREIGN_ACCESS_BITS,
                AclRule::NoAccess,
                "the backup destination must be a private folder: owned by the Hub's user, \
                 with no access for other accounts by its mode (use 0700) or by an access \
                 control list",
            ),
        };
        admit_held_directory(&fd, &canonical, bits, rule).map_err(|error| match error {
            DataRecoveryError::InvalidBackup(_) => invalid(message),
            other => other,
        })?;
    }
    Ok(AdmittedDirectory {
        fd,
        path: canonical,
        trust,
    })
}

/// Refuse an admitted destination that is the live data directory or lies
/// inside it. The ancestry is that of the directory held open, read by
/// walking `..` from its descriptor, so it holds whatever its old path names
/// now. Renaming the held directory into the live data directory needs write
/// access to that private directory.
pub(crate) fn refuse_inside(admitted: &AdmittedDirectory, live: &Path) -> Result<(), DataRecoveryError> {
    let live = stat(live).map_err(|source| super::io_error("inspecting live Hub data directory", live, source.into()))?;
    let unreadable = |source: rustix::io::Errno| DataRecoveryError::DestinationUnavailable {
        path: admitted.path.clone(),
        source: source.into(),
    };
    let mut ancestor = String::from(".");
    let mut below = None;
    for _ in 0..MAX_DESTINATION_DEPTH {
        let current = statat(&admitted.fd, ancestor.as_str(), AtFlags::empty()).map_err(unreadable)?;
        if current.st_dev == live.st_dev && current.st_ino == live.st_ino {
            return Err(invalid(
                "backup destination must not be inside the live Hub data directory",
            ));
        }
        let identity = (current.st_dev, current.st_ino);
        // The root is its own parent.
        if below == Some(identity) {
            return Ok(());
        }
        below = Some(identity);
        ancestor = if ancestor == "." { String::from("..") } else { format!("{ancestor}/..") };
    }
    Err(invalid("the backup destination is nested too deeply to check"))
}

/// Admit a directory the Hub made for its own removals: owned by the Hub's
/// user, no access for anyone else by mode, and no ACL write grant.
pub(crate) fn admit_protected_directory(fd: &OwnedFd, path: &Path) -> Result<(), DataRecoveryError> {
    admit_held_directory(fd, path, FOREIGN_ACCESS_BITS, AclRule::NoWrite)
}

fn admit_held_directory(
    fd: &OwnedFd,
    path: &Path,
    forbidden_bits: u32,
    rule: AclRule,
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
    refuse_foreign_acl(fd.as_fd(), &metadata, path, rule)
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
        refuse_foreign_acl(file.as_fd(), &metadata, path, AclRule::NoWrite)?;
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

/// The rights of each `allow` entry in an `ls -le` listing. The first line
/// is the object itself; each ACL entry follows on its own line, numbered
/// `0:`, `1:`..., ending in `allow <rights>` or `deny <rights>` (an inherited
/// entry says `inherited` before it).
#[cfg(any(target_os = "macos", test))]
fn acl_allow_entries(listing: &str) -> impl Iterator<Item = Vec<&str>> {
    listing.lines().skip(1).filter_map(|line| {
        let line = line.trim_start();
        let digits = line.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || !line[digits..].starts_with(':') {
            return None;
        }
        let tokens: Vec<&str> = line[digits + 1..].split_whitespace().collect();
        let position = tokens.iter().rposition(|token| *token == "allow")?;
        Some(
            tokens[position + 1..]
                .iter()
                .flat_map(|rights| rights.split(','))
                .map(str::trim)
                .collect(),
        )
    })
}

/// Whether an `ls -le` listing holds an `allow` entry with an unsafe right.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn acl_listing_grants_write(listing: &str) -> bool {
    acl_allow_entries(listing).any(|rights| rights.iter().any(|right| UNSAFE_ACL_RIGHTS.contains(right)))
}

/// Whether an `ls -le` listing holds any `allow` entry: on an owner-only
/// folder, every grant is access for someone else.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn acl_listing_grants_access(listing: &str) -> bool {
    acl_allow_entries(listing).next().is_some()
}

/// Refuse the held object when its ACL lets anyone write, add or delete
/// (or, under [`AclRule::NoAccess`], grants anything at all).
/// macOS has no safe descriptor-based ACL call and the crate forbids unsafe
/// code, so `ls -lde` lists the held object by its identity on its volume,
/// `/.vol/<device>/<inode>`. The kernel resolves that path by file ID, through
/// no directory, so renaming an ancestor cannot put another object under it;
/// the held descriptor keeps the inode allocated, so its number cannot pass to
/// another file while it is listed. A volume without file-ID lookup fails the
/// listing, and the object is refused.
#[cfg(target_os = "macos")]
fn refuse_foreign_acl(
    fd: BorrowedFd<'_>,
    held: &fs::Metadata,
    path: &Path,
    rule: AclRule,
) -> Result<(), DataRecoveryError> {
    use std::process::{Command, Stdio};

    let refuse = || {
        invalid(format!(
            "an access control list on {} lets another account {} it, or its access control \
             list could not be read",
            path.display(),
            match rule {
                AclRule::NoWrite => "write to",
                AclRule::NoAccess => "reach",
            }
        ))
    };
    let same = |metadata: &fs::Metadata| metadata.dev() == held.dev() && metadata.ino() == held.ino();
    // `held` describes the descriptor's object.
    let now = File::from(fd.try_clone_to_owned().map_err(|_| refuse())?)
        .metadata()
        .map_err(|_| refuse())?;
    if !same(&now) {
        return Err(refuse());
    }
    let identity = PathBuf::from(format!("/.vol/{}/{}", held.dev(), held.ino()));
    #[cfg(test)]
    test_seam::acl_listing("before_listing");
    let output = Command::new("/bin/ls")
        .arg("-lde")
        .arg(&identity)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| refuse())?;
    #[cfg(test)]
    test_seam::acl_listing("after_listing");
    let listed = fs::symlink_metadata(&identity).map_err(|_| refuse())?;
    if !output.status.success() || !same(&listed) {
        return Err(refuse());
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let granted = match rule {
        AclRule::NoWrite => acl_listing_grants_write(&listing),
        AclRule::NoAccess => acl_listing_grants_access(&listing),
    };
    if granted {
        return Err(refuse());
    }
    Ok(())
}

/// Elsewhere a POSIX ACL's mask is the group bits, which the mode check
/// already requires to be free of write access (or, owner-only, of any).
#[cfg(not(target_os = "macos"))]
#[allow(clippy::unnecessary_wraps)]
fn refuse_foreign_acl(
    _fd: BorrowedFd<'_>,
    _held: &fs::Metadata,
    _path: &Path,
    _rule: AclRule,
) -> Result<(), DataRecoveryError> {
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_seam {
    use std::cell::{Cell, RefCell};

    use super::VolumeTrust;

    type AclHook = Box<dyn FnMut(&str)>;

    thread_local! {
        static FORCED: Cell<Option<VolumeTrust>> = const { Cell::new(None) };
        static ACL_HOOK: RefCell<Option<AclHook>> = const { RefCell::new(None) };
        static DESTINATION_HOOK: RefCell<Option<AclHook>> = const { RefCell::new(None) };
    }

    /// Run `hook` on this thread just before ("before_open") and just after
    /// ("after_open") a destination is opened, until reset with `None`.
    pub(crate) fn on_destination<F: FnMut(&str) + 'static>(hook: Option<F>) {
        DESTINATION_HOOK.with(|slot| {
            *slot.borrow_mut() = hook.map(|hook| Box::new(hook) as AclHook);
        });
    }

    pub(super) fn destination(stage: &str) {
        DESTINATION_HOOK.with(|slot| {
            if let Some(hook) = slot.borrow_mut().as_mut() {
                hook(stage);
            }
        });
    }

    /// Run `hook` on this thread just before ("before_listing") and just
    /// after ("after_listing") each ACL listing, until reset with `None`.
    pub(crate) fn on_acl_listing<F: FnMut(&str) + 'static>(hook: Option<F>) {
        ACL_HOOK.with(|slot| {
            *slot.borrow_mut() = hook.map(|hook| Box::new(hook) as AclHook);
        });
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) fn acl_listing(stage: &str) {
        ACL_HOOK.with(|slot| {
            if let Some(hook) = slot.borrow_mut().as_mut() {
                hook(stage);
            }
        });
    }

    /// Classify every volume as `trust` on this thread until reset.
    pub(crate) fn force(trust: Option<VolumeTrust>) {
        FORCED.with(|slot| slot.set(trust));
    }

    pub(super) fn forced() -> Option<VolumeTrust> {
        FORCED.with(Cell::get)
    }
}
