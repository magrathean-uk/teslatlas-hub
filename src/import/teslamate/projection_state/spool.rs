// SPDX-License-Identifier: AGPL-3.0-only

/// Remove only the v1 spool runs whose ownership can be proved without
/// following symlinks. The caller must already hold the Hub publication gate:
/// a live direct import holds that same gate for its complete capture and
/// publication lifetime, so every validated run observed here is stale.
///
/// Flat pre-v1 files remain deliberately out of scope. They have no durable
/// generation binding and are safer to leave alone than to guess about.
pub(crate) fn recover_stale_import_generation_spools(
    root: &Path,
) -> Result<Vec<Uuid>, TeslaMateProjectionStateError> {
    let root_path = root.to_path_buf();
    let root_fd = open_private_directory_fd(root, &root_path)?;
    let staging_path = root.join(STAGING_DIRECTORY);
    let staging_fd =
        match open_child_private_directory_fd(&root_fd, STAGING_DIRECTORY, &staging_path) {
            Ok(fd) => fd,
            Err(TeslaMateProjectionStateError::ScopedFilesystem { source, .. })
                if source == rustix::io::Errno::NOENT =>
            {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
    let namespace_path = staging_path.join(IMPORT_GENERATION_NAMESPACE);
    let namespace_fd = match open_child_private_directory_fd(
        &staging_fd,
        IMPORT_GENERATION_NAMESPACE,
        &namespace_path,
    ) {
        Ok(fd) => fd,
        Err(TeslaMateProjectionStateError::ScopedFilesystem { source, .. })
            if source == rustix::io::Errno::NOENT =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };

    // Preflight the whole namespace before unlinking anything. A malformed
    // sibling must leave every entry and every staging row intact.
    let mut validated = Vec::new();
    let entries = Dir::read_from(&namespace_fd).map_err(|source| {
        TeslaMateProjectionStateError::ScopedFilesystem {
            path: namespace_path.clone(),
            source,
        }
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: namespace_path.clone(),
            source,
        })?;
        let Ok(name) = entry.file_name().to_str() else {
            return Err(
                TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(
                    namespace_path.clone(),
                ),
            );
        };
        if matches!(name, "." | "..") {
            continue;
        }
        validated.push(validate_stale_import_run(
            &namespace_fd,
            &namespace_path,
            name,
        )?);
    }

    let mut reclaimed = Vec::with_capacity(validated.len());
    for stale in validated {
        // Reopen by the exact, validated child name. `NOFOLLOW` and the
        // checked private directory mode make a replacement fail closed.
        let run_path = namespace_path.join(&stale.directory_name);
        let run_fd =
            open_child_private_directory_fd(&namespace_fd, &stale.directory_name, &run_path)?;
        validate_owned_import_run_fd(&run_fd, &run_path, stale.run_id, Some(&stale.children))?;
        for child in &stale.children {
            unlinkat(&run_fd, child.as_str(), AtFlags::empty()).map_err(|source| {
                TeslaMateProjectionStateError::ScopedFilesystem {
                    path: run_path.join(child),
                    source,
                }
            })?;
        }
        unlinkat(
            &namespace_fd,
            stale.directory_name.as_str(),
            AtFlags::REMOVEDIR,
        )
        .map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: run_path,
            source,
        })?;
        reclaimed.push(stale.run_id);
    }
    Ok(reclaimed)
}

fn ensure_import_generation_namespace(
    root: &Path,
    run_id: Uuid,
) -> Result<PathBuf, TeslaMateProjectionStateError> {
    ensure_exact_private_directory(root, false)?;
    let staging = root.join(STAGING_DIRECTORY);
    ensure_exact_private_directory(&staging, true)?;
    let namespace = staging.join(IMPORT_GENERATION_NAMESPACE);
    ensure_exact_private_directory(&namespace, true)?;
    let canonical_namespace = fs::canonicalize(&namespace).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: namespace.clone(),
            source,
        }
    })?;
    let run_directory = canonical_namespace.join(run_id.to_string());
    match fs::symlink_metadata(&run_directory) {
        Ok(metadata) => {
            validate_exact_private_directory(&run_directory, &metadata)?;
            validate_owner_marker(&run_directory, run_id)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&run_directory).map_err(|source| {
                TeslaMateProjectionStateError::CreateDirectory {
                    path: run_directory.clone(),
                    source,
                }
            })?;
            if let Err(error) = set_exact_private_directory_permissions(&run_directory)
                .and_then(|()| write_owner_marker(&run_directory, run_id))
            {
                // Do not recursively remove a partially-created path. This
                // best-effort cleanup can only remove the exact empty/new
                // directory, leaving an unexpected entry for fail-closed
                // startup recovery.
                let _ = fs::remove_file(run_directory.join(OWNER_FILE_NAME));
                let _ = fs::remove_dir(&run_directory);
                return Err(error);
            }
        }
        Err(source) => {
            return Err(TeslaMateProjectionStateError::InspectPath {
                path: run_directory,
                source,
            });
        }
    }
    Ok(canonical_namespace)
}

fn write_owner_marker(
    run_directory: &Path,
    run_id: Uuid,
) -> Result<(), TeslaMateProjectionStateError> {
    let marker = TeslaMateProjectionStateOwner {
        schema: OWNER_SCHEMA,
        kind: OWNER_KIND.to_owned(),
        run_id: run_id.to_string(),
    };
    let encoded =
        serde_json::to_vec(&marker).map_err(TeslaMateProjectionStateError::SerializeOwner)?;
    let path = run_directory.join(OWNER_FILE_NAME);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file =
        options
            .open(&path)
            .map_err(|source| TeslaMateProjectionStateError::CreateFile {
                path: path.clone(),
                source,
            })?;
    file.write_all(&encoded)
        .map_err(|source| TeslaMateProjectionStateError::WriteOwnerMarker {
            path: path.clone(),
            source,
        })?;
    file.sync_all()
        .map_err(|source| TeslaMateProjectionStateError::WriteOwnerMarker {
            path: path.clone(),
            source,
        })?;
    let metadata = fs::symlink_metadata(&path).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: path.clone(),
            source,
        }
    })?;
    validate_exact_private_file(&path, &metadata)
}

fn validate_import_generation_transfer_path(
    path: &Path,
    ownership: &TeslaMateProjectionStateImportOwnership,
) -> Result<PathBuf, TeslaMateProjectionStateError> {
    if ownership.run_id.is_nil() || ownership.attempt_id.is_nil() {
        return Err(TeslaMateProjectionStateError::InvalidImportGenerationRunId);
    }
    let run_name = ownership.run_id.to_string();
    let expected_file = format!("{}.{STATE_FILE_EXTENSION}", ownership.attempt_id);
    let expected_path = ownership
        .namespace_root
        .join(&run_name)
        .join(&expected_file);
    if path != expected_path {
        return Err(
            TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf()),
        );
    }
    if ownership
        .namespace_root
        .file_name()
        .and_then(|name| name.to_str())
        != Some(IMPORT_GENERATION_NAMESPACE)
    {
        return Err(
            TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf()),
        );
    }
    validate_exact_private_directory(
        &ownership.namespace_root,
        &fs::symlink_metadata(&ownership.namespace_root).map_err(|source| {
            TeslaMateProjectionStateError::InspectPath {
                path: ownership.namespace_root.clone(),
                source,
            }
        })?,
    )?;
    let staging = ownership.namespace_root.parent().ok_or_else(|| {
        TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf())
    })?;
    if staging.file_name().and_then(|name| name.to_str()) != Some(STAGING_DIRECTORY) {
        return Err(
            TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf()),
        );
    }
    validate_exact_private_directory(
        staging,
        &fs::symlink_metadata(staging).map_err(|source| {
            TeslaMateProjectionStateError::InspectPath {
                path: staging.to_path_buf(),
                source,
            }
        })?,
    )?;
    let root = staging.parent().ok_or_else(|| {
        TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf())
    })?;
    validate_exact_private_directory(
        root,
        &fs::symlink_metadata(root).map_err(|source| {
            TeslaMateProjectionStateError::InspectPath {
                path: root.to_path_buf(),
                source,
            }
        })?,
    )?;
    let canonical_namespace = fs::canonicalize(&ownership.namespace_root).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: ownership.namespace_root.clone(),
            source,
        }
    })?;
    if canonical_namespace != ownership.namespace_root {
        return Err(
            TeslaMateProjectionStateError::ImportGenerationNamespaceChanged {
                expected: ownership.namespace_root.clone(),
                actual: canonical_namespace,
            },
        );
    }
    let run_directory = ownership.namespace_root.join(&run_name);
    let run_metadata = fs::symlink_metadata(&run_directory).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: run_directory.clone(),
            source,
        }
    })?;
    validate_exact_private_directory(&run_directory, &run_metadata)?;
    validate_owner_marker(&run_directory, ownership.run_id)?;
    let metadata = fs::symlink_metadata(path).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        }
    })?;
    validate_exact_private_file(path, &metadata)?;
    let canonical_file =
        fs::canonicalize(path).map_err(|source| TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        })?;
    let canonical_run = fs::canonicalize(&run_directory).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: run_directory,
            source,
        }
    })?;
    if canonical_file.parent() != Some(canonical_run.as_path())
        || canonical_file.file_name().and_then(|name| name.to_str()) != Some(expected_file.as_str())
    {
        return Err(
            TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf()),
        );
    }
    Ok(canonical_file)
}

fn validate_owner_marker(
    run_directory: &Path,
    expected_run_id: Uuid,
) -> Result<(), TeslaMateProjectionStateError> {
    let path = run_directory.join(OWNER_FILE_NAME);
    let fd = open(
        &path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
        path: path.clone(),
        source,
    })?;
    validate_owner_marker_file(std::fs::File::from(fd), &path, expected_run_id)
}

fn validate_owner_marker_file(
    mut file: std::fs::File,
    path: &Path,
    expected_run_id: Uuid,
) -> Result<(), TeslaMateProjectionStateError> {
    let metadata =
        file.metadata()
            .map_err(|source| TeslaMateProjectionStateError::InspectPath {
                path: path.to_path_buf(),
                source,
            })?;
    validate_exact_private_file(path, &metadata)?;
    let mut bytes = Vec::new();
    let mut limited = (&mut file).take(MAX_OWNER_MARKER_BYTES.saturating_add(1));
    limited.read_to_end(&mut bytes).map_err(|source| {
        TeslaMateProjectionStateError::ReadOwnerMarker {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if u64::try_from(bytes.len()).expect("marker length fits u64") > MAX_OWNER_MARKER_BYTES {
        return Err(TeslaMateProjectionStateError::InvalidOwnerMarker(
            path.to_path_buf(),
        ));
    }
    let owner: TeslaMateProjectionStateOwner = serde_json::from_slice(&bytes)
        .map_err(|_| TeslaMateProjectionStateError::InvalidOwnerMarker(path.to_path_buf()))?;
    if owner.schema != OWNER_SCHEMA
        || owner.kind != OWNER_KIND
        || owner.run_id != expected_run_id.to_string()
    {
        return Err(TeslaMateProjectionStateError::InvalidOwnerMarker(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

fn validate_stale_import_run(
    namespace_fd: &impl std::os::fd::AsFd,
    namespace_path: &Path,
    name: &str,
) -> Result<ValidatedStaleImportRun, TeslaMateProjectionStateError> {
    let run_id = parse_canonical_uuid_component(name, namespace_path)?;
    let run_path = namespace_path.join(name);
    let run_fd = open_child_private_directory_fd(namespace_fd, name, &run_path)?;
    let children = validate_owned_import_run_fd(&run_fd, &run_path, run_id, None)?;
    Ok(ValidatedStaleImportRun {
        run_id,
        directory_name: name.to_owned(),
        children,
    })
}

fn validate_owned_import_run_fd(
    run_fd: &impl std::os::fd::AsFd,
    run_path: &Path,
    expected_run_id: Uuid,
    expected_children: Option<&[String]>,
) -> Result<Vec<String>, TeslaMateProjectionStateError> {
    validate_private_directory_stat(
        run_path,
        fstat(run_fd).map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: run_path.to_path_buf(),
            source,
        })?,
    )?;
    let owner_path = run_path.join(OWNER_FILE_NAME);
    let owner_fd = openat(
        run_fd,
        OWNER_FILE_NAME,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
        path: owner_path.clone(),
        source,
    })?;
    validate_owner_marker_file(std::fs::File::from(owner_fd), &owner_path, expected_run_id)?;

    let mut children = Vec::new();
    let mut main_attempts = HashSet::new();
    let mut sidecar_attempts = Vec::new();
    let entries = Dir::read_from(run_fd).map_err(|source| {
        TeslaMateProjectionStateError::ScopedFilesystem {
            path: run_path.to_path_buf(),
            source,
        }
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: run_path.to_path_buf(),
            source,
        })?;
        let Ok(name) = entry.file_name().to_str() else {
            return Err(
                TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(
                    run_path.to_path_buf(),
                ),
            );
        };
        if matches!(name, "." | "..") {
            continue;
        }
        let path = run_path.join(name);
        if name == OWNER_FILE_NAME {
            let stat = statat(run_fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|source| {
                TeslaMateProjectionStateError::ScopedFilesystem {
                    path: path.clone(),
                    source,
                }
            })?;
            validate_private_regular_file_stat(&path, stat)?;
            children.push(name.to_owned());
            continue;
        }
        let (attempt_id, is_main) = parse_import_spool_child(name, run_path)?;
        let stat = statat(run_fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|source| {
            TeslaMateProjectionStateError::ScopedFilesystem {
                path: path.clone(),
                source,
            }
        })?;
        validate_private_regular_file_stat(&path, stat)?;
        if is_main {
            main_attempts.insert(attempt_id);
        } else {
            sidecar_attempts.push(attempt_id);
        }
        children.push(name.to_owned());
    }
    if !children.iter().any(|name| name == OWNER_FILE_NAME)
        || sidecar_attempts
            .iter()
            .any(|attempt| !main_attempts.contains(attempt))
    {
        return Err(
            TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(run_path.to_path_buf()),
        );
    }
    children.sort_unstable();
    if let Some(expected) = expected_children
        && children.as_slice() != expected
    {
        return Err(
            TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(run_path.to_path_buf()),
        );
    }
    Ok(children)
}

fn parse_canonical_uuid_component(
    value: &str,
    parent: &Path,
) -> Result<Uuid, TeslaMateProjectionStateError> {
    let id = Uuid::parse_str(value).map_err(|_| {
        TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(parent.join(value))
    })?;
    if id.is_nil() || id.to_string() != value {
        return Err(
            TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(parent.join(value)),
        );
    }
    Ok(id)
}

fn parse_import_spool_child(
    value: &str,
    parent: &Path,
) -> Result<(Uuid, bool), TeslaMateProjectionStateError> {
    let (stem, is_main) =
        if let Some(stem) = value.strip_suffix(&format!(".{STATE_FILE_EXTENSION}")) {
            (stem, true)
        } else if let Some(stem) = value.strip_suffix(SQLITE_JOURNAL_SUFFIX) {
            (stem, false)
        } else if let Some(stem) = value.strip_suffix(SQLITE_WAL_SUFFIX) {
            (stem, false)
        } else if let Some(stem) = value.strip_suffix(SQLITE_SHM_SUFFIX) {
            (stem, false)
        } else {
            return Err(
                TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(parent.join(value)),
            );
        };
    Ok((parse_canonical_uuid_component(stem, parent)?, is_main))
}

fn open_private_directory_fd(
    path: &Path,
    display_path: &Path,
) -> Result<rustix::fd::OwnedFd, TeslaMateProjectionStateError> {
    let fd = open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
        path: display_path.to_path_buf(),
        source,
    })?;
    validate_private_directory_stat(
        display_path,
        fstat(&fd).map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: display_path.to_path_buf(),
            source,
        })?,
    )?;
    Ok(fd)
}

fn open_child_private_directory_fd(
    parent_fd: &impl std::os::fd::AsFd,
    name: &str,
    display_path: &Path,
) -> Result<rustix::fd::OwnedFd, TeslaMateProjectionStateError> {
    let fd = openat(
        parent_fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
        path: display_path.to_path_buf(),
        source,
    })?;
    validate_private_directory_stat(
        display_path,
        fstat(&fd).map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: display_path.to_path_buf(),
            source,
        })?,
    )?;
    Ok(fd)
}

fn cleanup_empty_import_generation_run(
    ownership: &TeslaMateProjectionStateOwnership,
) -> Result<(), TeslaMateProjectionStateError> {
    let TeslaMateProjectionStateOwnership::ImportGeneration(ownership) = ownership else {
        return Ok(());
    };
    let run_directory = ownership.namespace_root.join(ownership.run_id.to_string());
    let metadata = match fs::symlink_metadata(&run_directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(TeslaMateProjectionStateError::InspectPath {
                path: run_directory,
                source,
            });
        }
    };
    validate_exact_private_directory(&run_directory, &metadata)?;
    validate_owner_marker(&run_directory, ownership.run_id)?;
    let mut entries = fs::read_dir(&run_directory).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: run_directory.clone(),
            source,
        }
    })?;
    let Some(entry) = entries.next() else {
        return Err(TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(run_directory));
    };
    let entry = entry.map_err(|source| TeslaMateProjectionStateError::InspectPath {
        path: run_directory.clone(),
        source,
    })?;
    if entry.file_name() != OWNER_FILE_NAME || entries.next().is_some() {
        return Ok(());
    }
    let marker = run_directory.join(OWNER_FILE_NAME);
    fs::remove_file(&marker).map_err(|source| TeslaMateProjectionStateError::RemoveFile {
        path: marker,
        source,
    })?;
    fs::remove_dir(&run_directory).map_err(|source| {
        TeslaMateProjectionStateError::RemoveDirectory {
            path: run_directory,
            source,
        }
    })
}

fn validate_exact_private_directory(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), TeslaMateProjectionStateError> {
    if metadata.file_type().is_symlink() {
        return Err(TeslaMateProjectionStateError::SymlinkPath(
            path.to_path_buf(),
        ));
    }
    if !metadata.is_dir() {
        return Err(TeslaMateProjectionStateError::ExpectedDirectory(
            path.to_path_buf(),
        ));
    }
    require_exact_private_permissions(path, metadata, 0o700)
}

fn validate_exact_private_file(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), TeslaMateProjectionStateError> {
    if metadata.file_type().is_symlink() {
        return Err(TeslaMateProjectionStateError::SymlinkPath(
            path.to_path_buf(),
        ));
    }
    if !metadata.is_file() {
        return Err(TeslaMateProjectionStateError::ExpectedFile(
            path.to_path_buf(),
        ));
    }
    require_exact_private_permissions(path, metadata, 0o600)
}

fn ensure_exact_private_directory(
    path: &Path,
    create_if_missing: bool,
) -> Result<(), TeslaMateProjectionStateError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_exact_private_directory(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_if_missing => {
            fs::create_dir(path).map_err(|source| {
                TeslaMateProjectionStateError::CreateDirectory {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
            set_exact_private_directory_permissions(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(
            TeslaMateProjectionStateError::ExpectedDirectory(path.to_path_buf()),
        ),
        Err(source) => Err(TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(unix)]
fn require_exact_private_permissions(
    path: &Path,
    metadata: &fs::Metadata,
    expected: u32,
) -> Result<(), TeslaMateProjectionStateError> {
    if metadata.permissions().mode() & 0o777 != expected {
        return Err(
            TeslaMateProjectionStateError::ImportGenerationPathNotPrivate(path.to_path_buf()),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_exact_private_permissions(
    _path: &Path,
    _metadata: &fs::Metadata,
    _expected: u32,
) -> Result<(), TeslaMateProjectionStateError> {
    Ok(())
}

#[cfg(unix)]
fn set_exact_private_directory_permissions(
    path: &Path,
) -> Result<(), TeslaMateProjectionStateError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        TeslaMateProjectionStateError::SetPermissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_exact_private_directory_permissions(
    _path: &Path,
) -> Result<(), TeslaMateProjectionStateError> {
    Ok(())
}

fn validate_private_directory_stat(
    path: &Path,
    stat: rustix::fs::Stat,
) -> Result<(), TeslaMateProjectionStateError> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (Mode::from_raw_mode(stat.st_mode).as_raw_mode() & 0o777) != 0o700
    {
        return Err(
            TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(path.to_path_buf()),
        );
    }
    Ok(())
}

fn validate_private_regular_file_stat(
    path: &Path,
    stat: rustix::fs::Stat,
) -> Result<(), TeslaMateProjectionStateError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || (Mode::from_raw_mode(stat.st_mode).as_raw_mode() & 0o777) != 0o600
    {
        return Err(
            TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(path.to_path_buf()),
        );
    }
    Ok(())
}

fn attached_transfer_path(
    connection: &Connection,
) -> Result<PathBuf, TeslaMateProjectionStateError> {
    let rows = connection
        .prepare("PRAGMA database_list")
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    let path = rows
        .into_iter()
        .find_map(|(schema, path)| {
            (schema == TESLAMATE_PROJECTION_STATE_ATTACHMENT_SCHEMA).then_some(PathBuf::from(path))
        })
        .ok_or(TeslaMateProjectionStateError::TransferAttachmentMissing)?;
    fs::canonicalize(&path)
        .map_err(|source| TeslaMateProjectionStateError::InspectPath { path, source })
}

fn validate_private_transfer_path(path: &Path) -> Result<PathBuf, TeslaMateProjectionStateError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if metadata.file_type().is_symlink() {
        return Err(TeslaMateProjectionStateError::SymlinkPath(
            path.to_path_buf(),
        ));
    }
    if !metadata.is_file() {
        return Err(TeslaMateProjectionStateError::ExpectedFile(
            path.to_path_buf(),
        ));
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some(STATE_FILE_EXTENSION) {
        return Err(TeslaMateProjectionStateError::InvalidTransferPath(
            path.to_path_buf(),
        ));
    }
    require_private_transfer_permissions(path, &metadata)?;

    let parent = path
        .parent()
        .ok_or_else(|| TeslaMateProjectionStateError::InvalidTransferPath(path.to_path_buf()))?;
    if parent.file_name().and_then(|name| name.to_str()) != Some(STAGING_DIRECTORY) {
        return Err(TeslaMateProjectionStateError::InvalidTransferPath(
            path.to_path_buf(),
        ));
    }
    let parent_metadata = fs::symlink_metadata(parent).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: parent.to_path_buf(),
            source,
        }
    })?;
    if parent_metadata.file_type().is_symlink() {
        return Err(TeslaMateProjectionStateError::SymlinkPath(
            parent.to_path_buf(),
        ));
    }
    if !parent_metadata.is_dir() {
        return Err(TeslaMateProjectionStateError::ExpectedDirectory(
            parent.to_path_buf(),
        ));
    }
    require_private_transfer_permissions(parent, &parent_metadata)?;

    let canonical_parent =
        fs::canonicalize(parent).map_err(|source| TeslaMateProjectionStateError::InspectPath {
            path: parent.to_path_buf(),
            source,
        })?;
    let canonical_path =
        fs::canonicalize(path).map_err(|source| TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        })?;
    if canonical_path.parent() != Some(canonical_parent.as_path())
        || canonical_path.file_name() != path.file_name()
    {
        return Err(TeslaMateProjectionStateError::InvalidTransferPath(
            path.to_path_buf(),
        ));
    }
    Ok(canonical_path)
}

#[cfg(unix)]
fn require_private_transfer_permissions(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), TeslaMateProjectionStateError> {
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(TeslaMateProjectionStateError::TransferPathNotPrivate(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_private_transfer_permissions(
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), TeslaMateProjectionStateError> {
    Ok(())
}
