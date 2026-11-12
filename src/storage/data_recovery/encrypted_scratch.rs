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
//! Only macOS provides the encrypted volume today; elsewhere a catalogue above
//! the cap is refused with a clear error until the Linux Hub gains one.
#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime},
};

use uuid::Uuid;
use zeroize::Zeroizing;

use super::{DataRecoveryError, PRIVATE_DIRECTORY_MODE, PRIVATE_FILE_MODE, invalid, io_error};

/// The private folder of one scratch volume in the user's temporary
/// directory: the image, the lock and the mount record.
const SCRATCH_PREFIX: &str = "teslatlas-hub-backup-scratch-";
const VOLUME_PREFIX: &str = "TeslatlasHubBackupScratch-";
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
    root: PathBuf,
    mount: PathBuf,
    /// Held (exclusively locked) for the scratch's whole life.
    lock: Option<File>,
}

impl EncryptedScratch {
    /// Create and attach a scratch volume that can hold `capacity_bytes`.
    /// Stale scratch volumes of killed runs are removed first.
    pub(super) fn create(capacity_bytes: u64) -> Result<Self, DataRecoveryError> {
        let parent = std::env::temp_dir();
        sweep_stale(&parent);
        create(&parent, capacity_bytes)
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
        // next run's sweep; its image stays encrypted.
        if detach(&self.mount) {
            remove_root(&self.root);
        }
        // The lock goes last, so a sweep never finds this folder unlocked
        // while its volume is still attached.
        drop(self.lock.take());
    }
}

#[cfg(not(target_os = "macos"))]
fn create(_parent: &Path, _capacity_bytes: u64) -> Result<EncryptedScratch, DataRecoveryError> {
    Err(invalid(format!(
        "the Hub catalogue is larger than the {} MiB that an encrypted backup may hold in \
         memory, and this platform has no encrypted scratch volume for it yet",
        super::encrypted_backup::MAX_CATALOGUE_IMAGE_BYTES / (1024 * 1024)
    )))
}

#[cfg(target_os = "macos")]
fn create(parent: &Path, capacity_bytes: u64) -> Result<EncryptedScratch, DataRecoveryError> {
    let id = Uuid::new_v4().simple().to_string();
    let root = parent.join(format!("{SCRATCH_PREFIX}{id}"));
    fs::DirBuilder::new()
        .mode(PRIVATE_DIRECTORY_MODE)
        .create(&root)
        .map_err(|source| io_error("creating the backup scratch folder", &root, source))?;
    let lock_path = root.join(LOCK_NAME);
    let lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(&lock_path)
        .map_err(|source| io_error("creating the backup scratch lock", &lock_path, source));
    let lock = match lock {
        Ok(lock) => lock,
        Err(error) => {
            remove_root(&root);
            return Err(error);
        }
    };
    if lock.try_lock().is_err() {
        remove_root(&root);
        return Err(invalid("the backup scratch lock is held by another process"));
    }
    match attach_new_volume(&root, &id, capacity_bytes) {
        Ok(mount) => Ok(EncryptedScratch {
            root,
            mount,
            lock: Some(lock),
        }),
        Err(error) => {
            if let Some(mount) = read_mount_record(&root) {
                detach(&mount);
            }
            remove_root(&root);
            Err(error)
        }
    }
}

#[cfg(target_os = "macos")]
fn attach_new_volume(
    root: &Path,
    id: &str,
    capacity_bytes: u64,
) -> Result<PathBuf, DataRecoveryError> {
    let image = root.join(IMAGE_NAME);
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
    set_private_file_mode(&image)?;
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
    fs::write(&record, mount.as_os_str().as_encoded_bytes())
        .map_err(|source| io_error("recording the backup scratch mount", &record, source))?;
    set_private_file_mode(&record)?;
    // The volume must be a separate, empty file system that only this user
    // can enter.
    let mounted = fs::symlink_metadata(&mount)
        .map_err(|source| io_error("inspecting the backup scratch volume", &mount, source))?;
    let holder = fs::metadata(root)
        .map_err(|source| io_error("inspecting the backup scratch folder", root, source))?;
    if !mounted.is_dir()
        || mounted.dev() == holder.dev()
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

#[cfg(target_os = "macos")]
fn set_private_file_mode(path: &Path) -> Result<(), DataRecoveryError> {
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE))
        .map_err(|source| io_error("protecting a backup scratch file", path, source))
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

/// A recorded mount point, accepted only when it names one of the Hub's own
/// scratch volumes.
fn read_mount_record(root: &Path) -> Option<PathBuf> {
    let bytes = fs::read(root.join(MOUNT_RECORD)).ok()?;
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

fn remove_root(root: &Path) {
    for name in [IMAGE_NAME, MOUNT_RECORD, LOCK_NAME] {
        let _ = fs::remove_file(root.join(name));
    }
    let _ = fs::remove_dir(root);
}

/// Detach and remove the scratch volumes of killed runs: a scratch folder of
/// this user whose lock nobody holds. A folder whose lock is held belongs to a
/// live run and stays.
fn sweep_stale(parent: &Path) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let uid = rustix::process::getuid().as_raw();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name
            .to_str()
            .is_some_and(|name| name.starts_with(SCRATCH_PREFIX))
        {
            continue;
        }
        let root = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&root) else {
            continue;
        };
        if !metadata.is_dir() || metadata.uid() != uid {
            continue;
        }
        let lock = match File::options().write(true).open(root.join(LOCK_NAME)) {
            Ok(lock) => lock,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let old = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                    .is_some_and(|age| age > ORPHAN_AGE);
                if old {
                    remove_root(&root);
                }
                continue;
            }
            Err(_) => continue,
        };
        if lock.try_lock().is_err() {
            continue;
        }
        let detached = read_mount_record(&root).is_none_or(|mount| detach(&mount));
        if detached {
            remove_root(&root);
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
        read_mount_record(root)
    }
}
