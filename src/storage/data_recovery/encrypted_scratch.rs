// SPDX-License-Identifier: AGPL-3.0-only

//! The encrypted temporary copy for catalogues above the in-memory cap
//! (M06-T07, DEC-079).
//!
//! A catalogue larger than [`super::encrypted_backup::MAX_CATALOGUE_IMAGE_BYTES`]
//! cannot be copied, sealed and serialised in memory within the Hub's 1 GiB
//! peak, so it is copied into a private scratch volume whose blocks are
//! encrypted on disk: a sparse disk image with AES-256 encryption, created
//! and attached with a passphrase that exists only in this process (random,
//! handed to `hdiutil` on standard input, never stored), attached without
//! browsing (no Finder, no Spotlight) and owned by the Hub's user with its
//! root at mode 0700. SQLite reads and writes plain pages through the
//! attached volume; the disk only ever holds the image's encrypted blocks. The
//! volume is detached and the image removed when the run ends, whichever way
//! it ends. A killed run leaves its image behind (still encrypted, and its key
//! died with the process); the next run detaches and removes it, guided by a
//! lock that a live run holds for as long as its scratch exists.
//!
//! The folder that holds the scratch is the one admitted for the run, held
//! open from its admission (M06-T21): the sweep, the private folder, its lock
//! and mount record, and their removal are relative to that descriptor, and
//! hdiutil reaches the image through the held folder's identity path
//! (`/.vol/<device>/<inode>`), so a folder renamed or replaced by a link
//! after its admission is never followed.
//!
//! Only macOS provides the encrypted volume today; elsewhere a catalogue above
//! the cap is refused with a clear error until the Linux Hub gains one.
#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::OwnedFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime},
};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, mkdirat, openat, statat, unlinkat};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{
    DataRecoveryError, PRIVATE_DIRECTORY_MODE, backup_admission::AdmittedDirectory, invalid,
    io_error,
};

/// The private folder of one scratch volume in the user's temporary
/// directory: the image, the lock and the mount record.
const SCRATCH_PREFIX: &str = "teslatlas-hub-backup-scratch-"; // rename:keep
const VOLUME_PREFIX: &str = "TeslatlasHubBackupScratch-"; // rename:keep
const IMAGE_NAME: &str = "scratch.sparseimage";
const LOCK_NAME: &str = "lock";
const MOUNT_RECORD: &str = "mount";
#[cfg(target_os = "macos")]
const HDIUTIL: &str = "/usr/bin/hdiutil";
/// A scratch folder without a lock is someone's run starting up, unless it is
/// this old.
const ORPHAN_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// One attached encrypted scratch volume.
pub(super) struct EncryptedScratch {
    /// The approved folder, held open since its admission (M06-T21).
    parent: OwnedFd,
    /// The private folder's name in `parent`, and the folder, held open.
    name: String,
    folder: OwnedFd,
    /// The private folder's path when it was made, for messages and tests;
    /// nothing is reached through it.
    root: PathBuf,
    /// Empty until the volume is attached.
    mount: PathBuf,
    /// Held (exclusively locked) for the scratch's whole life.
    lock: Option<File>,
}

impl EncryptedScratch {
    /// Create and attach a scratch volume that can hold `capacity_bytes` in
    /// the user's temporary folder. Stale scratch volumes of killed runs are
    /// removed first.
    #[cfg(test)]
    pub(super) fn create(capacity_bytes: u64) -> Result<Self, DataRecoveryError> {
        let path = std::env::temp_dir();
        let fd = rustix::fs::open(
            &path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(at_error("opening the temporary folder", &path))?;
        let parent = AdmittedDirectory {
            fd,
            path,
            trust: super::backup_admission::VolumeTrust::Posix,
        };
        Self::create_in(&parent, capacity_bytes)
    }

    /// [`Self::create`] in `parent`, the run's admitted scratch location
    /// (M06-T21), through its held descriptor; stale scratch volumes of
    /// killed runs there go first.
    pub(super) fn create_in(
        parent: &AdmittedDirectory,
        capacity_bytes: u64,
    ) -> Result<Self, DataRecoveryError> {
        let held = parent
            .fd
            .try_clone()
            .map_err(|source| io_error("holding the backup scratch folder", &parent.path, source))?;
        sweep_stale(&held);
        create(held, &parent.path, capacity_bytes)
    }

    /// A path inside the attached volume.
    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.mount.join(name)
    }

    /// The private folder that holds the encrypted image.
    #[cfg(test)]
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// The encrypted image file: what the disk holds.
    #[cfg(test)]
    pub(super) fn image(&self) -> PathBuf {
        self.root.join(IMAGE_NAME)
    }

    /// Where the decrypted view of the volume is attached.
    #[cfg(test)]
    pub(super) fn mount(&self) -> &Path {
        &self.mount
    }
}

impl Drop for EncryptedScratch {
    fn drop(&mut self) {
        // A volume that cannot be detached now (still busy) is left to the
        // next run's sweep; its image stays encrypted. One whose attachment
        // failed part-way is found through its mount record.
        let mount = if self.mount.as_os_str().is_empty() {
            read_mount_record(&self.folder)
        } else {
            Some(self.mount.clone())
        };
        if mount.is_none_or(|mount| detach(&mount)) {
            remove_root(&self.parent, &self.name, &self.folder);
        }
        // The lock goes last, so a sweep never finds this folder unlocked
        // while its volume is still attached.
        drop(self.lock.take());
    }
}

#[cfg(not(target_os = "macos"))]
fn create(
    _parent: OwnedFd,
    _parent_path: &Path,
    _capacity_bytes: u64,
) -> Result<EncryptedScratch, DataRecoveryError> {
    Err(invalid(format!(
        "the Hub catalogue is larger than the {} MiB that an encrypted backup may hold in \
         memory, and this platform has no encrypted scratch volume for it yet",
        super::encrypted_backup::MAX_CATALOGUE_IMAGE_BYTES / (1024 * 1024)
    )))
}

#[cfg(target_os = "macos")]
fn create(
    parent: OwnedFd,
    parent_path: &Path,
    capacity_bytes: u64,
) -> Result<EncryptedScratch, DataRecoveryError> {
    let id = Uuid::new_v4().simple().to_string();
    let name = format!("{SCRATCH_PREFIX}{id}");
    let root = parent_path.join(&name);
    mkdirat(&parent, name.as_str(), Mode::RWXU)
        .map_err(at_error("creating the backup scratch folder", &root))?;
    let folder = match openat(&parent, name.as_str(), FOLDER_FLAGS, Mode::empty()) {
        Ok(folder) => folder,
        Err(source) => {
            let _ = unlinkat(&parent, name.as_str(), AtFlags::REMOVEDIR);
            return Err(io_error("opening the backup scratch folder", &root, source.into()));
        }
    };
    // From here a failure unwinds through Drop: detach what was attached and
    // remove the folder.
    let mut scratch = EncryptedScratch {
        parent,
        name,
        folder,
        root,
        mount: PathBuf::new(),
        lock: None,
    };
    let lock = openat(
        &scratch.folder,
        LOCK_NAME,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(at_error("creating the backup scratch lock", &scratch.root.join(LOCK_NAME)))?;
    let lock = File::from(lock);
    if lock.try_lock().is_err() {
        return Err(invalid("the backup scratch lock is held by another process"));
    }
    scratch.lock = Some(lock);
    scratch.mount = attach_new_volume(&scratch.folder, &scratch.root, &id, capacity_bytes)?;
    Ok(scratch)
}

const FOLDER_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn at_error<'a>(
    operation: &'static str,
    path: &'a Path,
) -> impl Fn(rustix::io::Errno) -> DataRecoveryError + 'a {
    move |source| io_error(operation, path, source.into())
}

/// The held folder's identity path, `/.vol/<device>/<inode>`. hdiutil takes
/// a path, and this one names the held folder itself, whatever has become of
/// the path it was made under (REV-CX-B3-STORAGE F3). A volume that cannot
/// name its folders this way is refused.
#[cfg(target_os = "macos")]
fn identity_path(folder: &OwnedFd, root: &Path) -> Result<PathBuf, DataRecoveryError> {
    let held = fstat(folder).map_err(at_error("inspecting the backup scratch folder", root))?;
    let path = PathBuf::from(format!("/.vol/{}/{}", held.st_dev, held.st_ino));
    match rustix::fs::stat(&path) {
        Ok(named) if named.st_dev == held.st_dev && named.st_ino == held.st_ino => Ok(path),
        _ => Err(invalid(
            "the backup scratch folder is on a volume that cannot name it by its identity; \
             choose a scratch folder on an APFS or Mac OS Extended volume",
        )),
    }
}

#[cfg(target_os = "macos")]
fn attach_new_volume(
    folder: &OwnedFd,
    root: &Path,
    id: &str,
    capacity_bytes: u64,
) -> Result<PathBuf, DataRecoveryError> {
    let image = identity_path(folder, root)?.join(IMAGE_NAME);
    let volume = format!("{VOLUME_PREFIX}{}", &id[..16]);
    // 32 random bytes, hex: the volume's key material lives only here and in
    // hdiutil while it runs.
    let mut entropy = Zeroizing::new([0_u8; 32]);
    getrandom::fill(entropy.as_mut_slice())
        .map_err(|error| invalid(format!("system randomness is unavailable: {error}")))?;
    let passphrase = Zeroizing::new(hex::encode(entropy.as_slice()));
    let size_mib = capacity_bytes.div_ceil(1024 * 1024).max(64);
    hdiutil(
        &[
            "create",
            "-quiet",
            "-size",
            &format!("{size_mib}m"),
            "-type",
            "SPARSE",
            "-fs",
            "APFS",
            "-volname",
            &volume,
            "-encryption",
            "AES-256",
            "-stdinpass",
        ],
        &image,
        passphrase.as_bytes(),
        "creating the encrypted backup scratch image",
    )?;
    protect_file(folder, IMAGE_NAME, root)?;
    let output = hdiutil(
        &[
            "attach",
            "-nobrowse",
            "-noautoopen",
            "-noverify",
            "-owners",
            "on",
            "-stdinpass",
        ],
        &image,
        passphrase.as_bytes(),
        "attaching the encrypted backup scratch image",
    )?;
    let mount = mount_point(&output, &volume).ok_or_else(|| {
        invalid("the encrypted backup scratch volume was attached without a mount point")
    })?;
    // Recorded before anything is written there, so a later sweep can detach
    // it if this process dies.
    let record = root.join(MOUNT_RECORD);
    let file = openat(
        folder,
        MOUNT_RECORD,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(at_error("recording the backup scratch mount", &record))?;
    File::from(file)
        .write_all(mount.as_os_str().as_encoded_bytes())
        .map_err(|source| io_error("recording the backup scratch mount", &record, source))?;
    // The volume must be a separate, empty file system that only this user
    // can enter.
    let mounted = fs::symlink_metadata(&mount)
        .map_err(|source| io_error("inspecting the backup scratch volume", &mount, source))?;
    let holder = fstat(folder).map_err(at_error("inspecting the backup scratch folder", root))?;
    if !mounted.is_dir()
        || mounted.dev() == holder.st_dev as u64
        || mounted.uid() != rustix::process::getuid().as_raw()
    {
        return Err(invalid(
            "the encrypted backup scratch volume is not a private volume of this user",
        ));
    }
    fs::set_permissions(&mount, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
        .map_err(|source| io_error("protecting the backup scratch volume", &mount, source))?;
    Ok(mount)
}

/// Make `name` in the held folder readable and writable by its owner only.
#[cfg(target_os = "macos")]
fn protect_file(folder: &OwnedFd, name: &str, root: &Path) -> Result<(), DataRecoveryError> {
    let path = root.join(name);
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file = openat(folder, name, flags, Mode::empty())
        .map_err(at_error("protecting a backup scratch file", &path))?;
    rustix::fs::fchmod(&file, Mode::RUSR | Mode::WUSR)
        .map_err(at_error("protecting a backup scratch file", &path))
}

/// Run hdiutil with `passphrase` on standard input; returns its output.
#[cfg(target_os = "macos")]
fn hdiutil(
    arguments: &[&str],
    image: &Path,
    passphrase: &[u8],
    operation: &'static str,
) -> Result<String, DataRecoveryError> {
    let mut child = Command::new(HDIUTIL)
        .args(arguments)
        .arg(image)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| io_error(operation, image, source))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(passphrase)
            .map_err(|source| io_error(operation, image, source))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|source| io_error(operation, image, source))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail
            .lines()
            .filter(|line| !line.contains("deprecated"))
            .collect::<Vec<_>>()
            .join(" ");
        return Err(io_error(
            operation,
            image,
            io::Error::other(format!("hdiutil failed ({}): {}", output.status, detail.trim())),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The mount point hdiutil printed for the volume named `volume`.
#[cfg(target_os = "macos")]
fn mount_point(output: &str, volume: &str) -> Option<PathBuf> {
    output.lines().find_map(|line| {
        let field = line.split('\t').map(str::trim).next_back()?;
        let path = Path::new(field);
        (path.is_absolute()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(volume)))
        .then(|| path.to_path_buf())
    })
}

/// A recorded mount point in the held scratch folder, accepted only when it
/// names one of the Hub's own scratch volumes.
fn read_mount_record(folder: &OwnedFd) -> Option<PathBuf> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file = openat(folder, MOUNT_RECORD, flags, Mode::empty()).ok()?;
    let mut bytes = Vec::new();
    File::from(file).read_to_end(&mut bytes).ok()?;
    let mount = PathBuf::from(String::from_utf8(bytes).ok()?);
    let name = mount.file_name()?.to_str()?;
    (mount.is_absolute() && name.starts_with(VOLUME_PREFIX)).then_some(mount)
}

/// Detach the volume at `mount`; true when nothing is attached there any
/// more.
#[cfg(target_os = "macos")]
fn detach(mount: &Path) -> bool {
    let attached = |mount: &Path| {
        match (fs::symlink_metadata(mount), mount.parent().map(fs::metadata)) {
            (Ok(volume), Some(Ok(parent))) => volume.dev() != parent.dev(),
            _ => false,
        }
    };
    for force in [false, true, true] {
        if !attached(mount) {
            return true;
        }
        let mut command = Command::new(HDIUTIL);
        command.args(["detach", "-quiet"]);
        if force {
            command.arg("-force");
        }
        let _ = command
            .arg(mount)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    !attached(mount)
}

#[cfg(not(target_os = "macos"))]
fn detach(_mount: &Path) -> bool {
    true
}

/// Remove the held scratch folder `folder`, named `name` in `parent`, and
/// its files. A name that now leads somewhere else is left alone.
fn remove_root(parent: &OwnedFd, name: &str, folder: &OwnedFd) {
    for entry in [IMAGE_NAME, MOUNT_RECORD, LOCK_NAME] {
        let _ = unlinkat(folder, entry, AtFlags::empty());
    }
    if let (Ok(named), Ok(held)) = (statat(parent, name, AtFlags::SYMLINK_NOFOLLOW), fstat(folder))
        && (named.st_dev, named.st_ino) == (held.st_dev, held.st_ino)
    {
        let _ = unlinkat(parent, name, AtFlags::REMOVEDIR);
    }
}

/// Detach and remove the scratch volumes of killed runs in the held folder
/// `parent`: a scratch folder of this user whose lock nobody holds. A folder
/// whose lock is held belongs to a live run and stays.
fn sweep_stale(parent: &OwnedFd) {
    let Ok(entries) = Dir::read_from(parent) else {
        return;
    };
    let uid = rustix::process::getuid().as_raw();
    for entry in entries.flatten() {
        let Some(name) = entry
            .file_name()
            .to_str()
            .ok()
            .filter(|name| name.starts_with(SCRATCH_PREFIX))
            .map(str::to_owned)
        else {
            continue;
        };
        let Ok(metadata) = statat(parent, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
        if FileType::from_raw_mode(metadata.st_mode) != FileType::Directory
            || metadata.st_uid != uid
        {
            continue;
        }
        let Ok(folder) = openat(parent, name.as_str(), FOLDER_FLAGS, Mode::empty()) else {
            continue;
        };
        let flags = OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let lock = match openat(&folder, LOCK_NAME, flags, Mode::empty()) {
            Ok(lock) => File::from(lock),
            Err(rustix::io::Errno::NOENT) => {
                let old = u64::try_from(metadata.st_mtime)
                    .ok()
                    .and_then(|seconds| {
                        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
                    })
                    .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                    .is_some_and(|age| age > ORPHAN_AGE);
                if old {
                    remove_root(parent, &name, &folder);
                }
                continue;
            }
            Err(_) => continue,
        };
        if lock.try_lock().is_err() {
            continue;
        }
        let detached = read_mount_record(&folder).is_none_or(|mount| detach(&mount));
        if detached {
            remove_root(parent, &name, &folder);
        }
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    /// Leave `scratch` as a killed process would: attached, its image and
    /// lock file in place, its lock released.
    pub(in crate::storage::data_recovery) fn abandon(mut scratch: EncryptedScratch) -> PathBuf {
        let root = scratch.root.clone();
        drop(scratch.lock.take());
        std::mem::forget(scratch);
        root
    }

    pub(in crate::storage::data_recovery) fn mount_of(root: &Path) -> Option<PathBuf> {
        let folder = rustix::fs::open(root, FOLDER_FLAGS, Mode::empty()).ok()?;
        read_mount_record(&folder)
    }
}
