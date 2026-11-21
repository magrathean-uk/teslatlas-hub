// SPDX-License-Identifier: AGPL-3.0-only

//! The kept archives a data backup carries (DEC-250). Each import keeps, per
//! vehicle, one compact archive and its source sessions under
//! `packs/physical-kept`, named by vehicle and generation, and the catalogue
//! records their digests (`physical_v3_kept:<vehicle>`). Every backup format
//! carries exactly the files the copied catalogue's archive records name, as
//! members `data/packs/physical-kept/<vehicle>.<generation>.kept` and
//! `.source.json`; every restore places them and requires the restored
//! directory to be exactly those files, private, at the recorded size and
//! digest. Records without an archive (legacy raw bases) name nothing a backup
//! carries: a restored legacy record withholds the vehicle's head until its
//! next import. Without archive records the directory is absent, so older
//! backups restore unchanged.

use super::*;
use crate::db::{PHYSICAL_V3_KEPT_DIRECTORY, PhysicalV3KeptFile, physical_v3_kept_files_in};

/// The member directory of the kept-archive files.
pub(super) const KEPT_ARCHIVE_DIRECTORY: &str = "data/packs/physical-kept";

/// The kept files a catalogue names, by file name.
pub(super) type KeptFiles = BTreeMap<String, PhysicalV3KeptFile>;

/// A canonical kept-file name: `<vehicle>.<generation>.kept` or
/// `<vehicle>.<generation>.source.json`, both UUIDs in canonical form.
fn canonical_kept_filename(name: &str) -> bool {
    let Some(stem) = name
        .strip_suffix(".kept")
        .or_else(|| name.strip_suffix(".source.json"))
    else {
        return false;
    };
    let canonical = |value: &str| Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value);
    let mut parts = stem.split('.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(vehicle), Some(generation), None) => canonical(vehicle) && canonical(generation),
        _ => false,
    }
}

/// The file name of a kept-archive member path, when it is canonical.
pub(super) fn kept_member_filename(path: &str) -> Option<&str> {
    let name = path.strip_prefix(&format!("{KEPT_ARCHIVE_DIRECTORY}/"))?;
    (!name.contains('/') && canonical_kept_filename(name)).then_some(name)
}

/// The kept files `connection`'s catalogue names.
pub(super) fn expected_kept_files_in(connection: &Connection) -> Result<KeptFiles, DataRecoveryError> {
    let mut files = KeptFiles::new();
    for file in physical_v3_kept_files_in(connection)? {
        if !canonical_kept_filename(&file.name) {
            return Err(invalid(format!("kept archive record names an unsafe file: {}", file.name)));
        }
        if files.insert(file.name.clone(), file).is_some() {
            return Err(invalid("kept archive records name one file twice"));
        }
    }
    Ok(files)
}

/// The kept files the catalogue at `data` names.
pub(super) fn expected_kept_files(data: &Path) -> Result<KeptFiles, DataRecoveryError> {
    expected_kept_files_in(&open_immutable_catalogue(data)?)
}

/// The member a kept file is in a backup, at `size` bytes.
pub(super) fn kept_member(file: &PhysicalV3KeptFile, size: u64) -> BackupMember {
    BackupMember {
        path: format!("{KEPT_ARCHIVE_DIRECTORY}/{}", file.name),
        size,
        mode: PRIVATE_FILE_MODE,
        sha256: file.sha256.clone(),
    }
}

fn kept_directory(data: &Path) -> PathBuf {
    data.join("packs").join(PHYSICAL_V3_KEPT_DIRECTORY)
}

/// The `packs` directory of a payload or restored tree holds exactly
/// `sha256`, `noop` when there are current no-ops, and `physical-kept` when
/// the catalogue names kept archives.
pub(super) fn require_packs_entries(
    data: &Path,
    has_noops: bool,
    has_kept: bool,
    description: &'static str,
) -> Result<(), DataRecoveryError> {
    let mut expected = BTreeSet::from(["sha256".to_owned()]);
    if has_noops {
        expected.insert("noop".to_owned());
    }
    if has_kept {
        expected.insert(PHYSICAL_V3_KEPT_DIRECTORY.to_owned());
    }
    require_directory_entry_set(&data.join("packs"), &expected, description)
}

/// The kept directory under `data` holds exactly `expected`: private, one
/// link each, at the recorded size where the record has one and the recorded
/// digest. Without expected files the directory must be absent. Returns the
/// files' total bytes.
pub(super) fn validate_kept_directory(
    data: &Path,
    expected: &KeptFiles,
) -> Result<u64, DataRecoveryError> {
    let directory = kept_directory(data);
    if expected.is_empty() {
        return match fs::symlink_metadata(&directory) {
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(0),
            Ok(_) => Err(invalid(
                "kept archive directory exists without a kept archive record",
            )),
            Err(source) => Err(io_error("inspecting kept archive directory", &directory, source)),
        };
    }
    let names = expected.keys().cloned().collect::<BTreeSet<_>>();
    require_directory_entry_set(&directory, &names, "kept archive directory")?;
    let mut bytes = 0_u64;
    for (name, file) in expected {
        let path = directory.join(name);
        let metadata = require_regular_file(&path, "kept archive member")?;
        if metadata.nlink() != 1
            || permission_mode(&metadata) != PRIVATE_FILE_MODE
            || file.bytes.is_some_and(|size| size != metadata.len())
            || sha256_file_hex(&path)? != file.sha256
        {
            return Err(invalid(format!(
                "kept archive member differs from its catalogue record: {KEPT_ARCHIVE_DIRECTORY}/{name}"
            )));
        }
        bytes = checked_capacity_add(bytes, metadata.len())?;
    }
    Ok(bytes)
}

/// Protect a staged backup payload's kept directory (0700, files 0600) and
/// check it against the copied catalogue.
pub(super) fn protect_kept_directory(payload: &Path) -> Result<(), DataRecoveryError> {
    let expected = expected_kept_files(payload)?;
    if !expected.is_empty() {
        let directory = kept_directory(payload);
        require_real_directory(&directory, "backup kept archive directory")?;
        set_mode(&directory, PRIVATE_DIRECTORY_MODE, "protecting backup kept archive directory")?;
        for name in expected.keys() {
            let path = directory.join(name);
            require_regular_file(&path, "backup kept archive member")?;
            set_mode(&path, PRIVATE_FILE_MODE, "protecting backup kept archive member")?;
        }
    }
    validate_kept_directory(payload, &expected)?;
    Ok(())
}

/// The listed members of a staged payload's kept files, each inventoried.
pub(super) fn inventory_kept_members(payload: &Path) -> Result<Vec<BackupMember>, DataRecoveryError> {
    let expected = expected_kept_files(payload)?;
    validate_kept_directory(payload, &expected)?;
    expected
        .keys()
        .map(|name| inventory_file(&kept_directory(payload).join(name), format!("{KEPT_ARCHIVE_DIRECTORY}/{name}")))
        .collect()
}

/// A listed format's kept members must be exactly the files the tree's
/// catalogue names, and the tree's kept directory exactly those files.
pub(super) fn validate_listed_kept_members(
    data: &Path,
    members: &[BackupMember],
    expected: &KeptFiles,
) -> Result<(), DataRecoveryError> {
    let declared = members
        .iter()
        .filter_map(|member| kept_member_filename(&member.path))
        .collect::<BTreeSet<_>>();
    if !declared.iter().copied().eq(expected.keys().map(String::as_str)) {
        return Err(invalid(
            "data-backup kept archive members do not match the catalogue",
        ));
    }
    validate_kept_directory(data, expected)?;
    Ok(())
}

/// Copy a directory backup's kept files into `staging` against their
/// records; returns how many were copied.
pub(super) fn copy_kept_directory(data: &Path, staging: &Path) -> Result<u64, DataRecoveryError> {
    let expected = expected_kept_files(data)?;
    if expected.is_empty() {
        return Ok(0);
    }
    create_private_directory(&kept_directory(staging))?;
    for (name, file) in &expected {
        let from = kept_directory(data).join(name);
        let metadata = require_regular_file(&from, "source backup member")?;
        let member = kept_member(file, file.bytes.unwrap_or(metadata.len()));
        copy_verified_member(&from, &kept_directory(staging).join(name), &member)?;
    }
    Ok(expected.len() as u64)
}

/// Sync a tree's kept directory and its files, when it has one.
pub(super) fn sync_kept_directory(data: &Path, has_kept: bool) -> Result<(), DataRecoveryError> {
    if !has_kept {
        return Ok(());
    }
    let directory = kept_directory(data);
    for entry in read_directory(&directory)? {
        sync_file(&entry.path())?;
    }
    sync_directory(&directory)
}

/// The kept files of the live store now, by count and bytes, for the
/// encrypted backup's space estimate before the gate (a file a concurrent
/// import has just removed counts as its recorded size, or nothing).
pub(super) fn live_kept_totals(store: &HubStore) -> Result<(u64, u64), DataRecoveryError> {
    let (mut count, mut bytes) = (0_u64, 0_u64);
    for file in store.physical_v3_kept_files()? {
        let size = match file.bytes {
            Some(size) => size,
            None => fs::symlink_metadata(
                store.packs_dir().join(PHYSICAL_V3_KEPT_DIRECTORY).join(&file.name),
            )
            .map_or(0, |metadata| metadata.len()),
        };
        count += 1;
        bytes = checked_capacity_add(bytes, size)?;
    }
    Ok((count, bytes))
}

/// One kept file an encrypted backup streams: its member and the live file,
/// opened under the publication gate so a later supersession, which only
/// unlinks it, cannot take it away; a change to its bytes fails the stream's
/// digest check.
pub(super) struct KeptBody {
    pub(super) member: BackupMember,
    pub(super) file: File,
}

/// Open every kept file the catalogue copy names in the live store, while
/// the gate holds their writers off. A missing file, or an archive whose
/// size is not its recorded one, fails the backup.
pub(super) fn open_kept_bodies(
    store: &HubStore,
    catalogue: &Connection,
) -> Result<Vec<KeptBody>, DataRecoveryError> {
    let directory = store.packs_dir().join(PHYSICAL_V3_KEPT_DIRECTORY);
    let mut bodies = Vec::new();
    for (name, file) in expected_kept_files_in(catalogue)? {
        let path = directory.join(&name);
        let input = open_regular_nonblocking(&path, "opening backup kept archive member")
            .map_err(|error| {
                invalid(format!(
                    "backup kept archive member the catalogue names cannot be read: {KEPT_ARCHIVE_DIRECTORY}/{name}: {error}"
                ))
            })?;
        let size = input
            .metadata()
            .map_err(|source| io_error("inspecting backup kept archive member", &path, source))?
            .len();
        if file.bytes.is_some_and(|expected| expected != size) {
            return Err(invalid(format!(
                "backup kept archive member differs from its catalogue record: {KEPT_ARCHIVE_DIRECTORY}/{name}"
            )));
        }
        bodies.push(KeptBody {
            member: kept_member(&file, size),
            file: input,
        });
    }
    Ok(bodies)
}
