// SPDX-License-Identifier: AGPL-3.0-only

fn verify_file(
    metadata: &TransportPack,
    path: &Path,
    limits: ProtocolLimits,
) -> Result<VerifiedTransportPack, ProjectionPackError> {
    let file = File::open(path).map_err(|source| ProjectionPackError::OpenCompressed {
        path: path.to_path_buf(),
        source,
    })?;
    metadata
        .verify_reader(file, limits)
        .map_err(ProjectionPackError::Protocol)
}

fn compress_file(
    source_path: &Path,
    destination_path: &Path,
) -> Result<(Sha256Digest, u64), ProjectionPackError> {
    compress_file_with_workers(source_path, destination_path, compression_worker_count())
}

const MAX_COMPRESSION_WORKERS: usize = 4;

fn compression_worker_count() -> u32 {
    let available = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    compression_worker_count_for(available)
}

fn compression_worker_count_for(available: usize) -> u32 {
    available.clamp(1, MAX_COMPRESSION_WORKERS) as u32
}

fn compress_file_with_workers(
    source_path: &Path,
    destination_path: &Path,
    workers: u32,
) -> Result<(Sha256Digest, u64), ProjectionPackError> {
    let mut source = File::open(source_path).map_err(|source| ProjectionPackError::ReadSource {
        path: source_path.to_path_buf(),
        source,
    })?;
    let destination = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(destination_path)
        .map_err(|source| ProjectionPackError::CreateCompressed {
            path: destination_path.to_path_buf(),
            source,
        })?;
    let mut encoder =
        zstd::stream::write::Encoder::new(HashingWriter::new(destination), COMPRESSION_LEVEL)
            .map_err(ProjectionPackError::Compress)?;
    encoder
        .multithread(workers)
        .map_err(ProjectionPackError::Compress)?;
    io::copy(&mut source, &mut encoder).map_err(ProjectionPackError::Compress)?;
    crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::PackCompressedWrite,
    )
    .map_err(ProjectionPackError::Durability)?;
    let (file, digest, bytes) = encoder
        .finish()
        .map_err(ProjectionPackError::Compress)?
        .finish();
    crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::PackCompressedFsync,
    )
    .map_err(ProjectionPackError::Durability)?;
    file.sync_all()
        .map_err(ProjectionPackError::SyncCompressed)?;
    Ok((digest, bytes))
}

fn available_bytes(path: &Path) -> Result<u64, ProjectionPackError> {
    let stats = statvfs(path).map_err(|source| ProjectionPackError::FilesystemSpace {
        path: path.to_path_buf(),
        source,
    })?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or(ProjectionPackError::CapacityOverflow)
}

struct ImmutablePublication {
    ownership: ProjectionPackOwnership,
    cleanup_state: ProjectionPackCleanupState,
}

fn publish_immutable(
    temporary: &mut StagedFile,
    final_path: &Path,
    metadata: &TransportPack,
    limits: ProtocolLimits,
) -> Result<ImmutablePublication, ProjectionPackError> {
    let temporary_path = temporary.path().to_path_buf();
    fs::set_permissions(
        &temporary_path,
        fs::Permissions::from_mode(SHARED_IMMUTABLE_PACK_MODE),
    )
    .map_err(|source| ProjectionPackError::Publish {
        path: temporary_path.to_path_buf(),
        source,
    })?;
    File::open(&temporary_path)
        .and_then(|file| file.sync_all())
        .map_err(|source| ProjectionPackError::Publish {
            path: temporary_path.to_path_buf(),
            source,
        })?;
    crate::durability_fault::check(crate::durability_fault::DurabilityFaultPoint::PackFinalInstall)
        .map_err(ProjectionPackError::Durability)?;
    let ownership = match fs::hard_link(&temporary_path, final_path) {
        Ok(()) => ProjectionPackOwnership::Created,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            verify_file(metadata, final_path, limits)
                .map(|_| ProjectionPackOwnership::ReusedExisting)?
        }
        Err(source) => Err(ProjectionPackError::Publish {
            path: final_path.to_path_buf(),
            source,
        })?,
    };
    // A prior attempt may have installed this exact immutable name and then
    // failed before syncing its parent directory. Reuse proves the bytes, not
    // the directory entry's durability, so both paths must cross the same
    // checkpoint and sync before publication can be reported complete.
    crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::PackFinalDirectoryFsync,
    )
    .map_err(ProjectionPackError::Durability)?;
    sync_parent_directory(final_path)?;
    // Once a final content name exists (or a verified identical object was
    // reused), an error in staging cleanup is a restart-repairable orphan.
    // Do not let `Drop` erase the evidence or pretend its directory entry was
    // durably removed.
    temporary.retain_for_repair();
    // The final immutable hard link and its parent have been synced.  Remove
    // the now-public staging name and sync that namespace too, so a normal
    // completed publication never leaves a 0640 temporary file behind.
    if let Err(source) = crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::PackStagingUnlink,
    ) {
        tracing::warn!(%source, path = %temporary_path.display(), "pack committed with staging cleanup pending");
        return Ok(ImmutablePublication {
            ownership,
            cleanup_state: ProjectionPackCleanupState::PendingStartupRepair,
        });
    }
    if let Err(source) = fs::remove_file(&temporary_path) {
        tracing::warn!(%source, path = %temporary_path.display(), "pack committed with staging cleanup pending");
        return Ok(ImmutablePublication {
            ownership,
            cleanup_state: ProjectionPackCleanupState::PendingStartupRepair,
        });
    }
    temporary.mark_removed();
    if let Err(source) = crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::PackStagingDirectoryFsync,
    ) {
        tracing::warn!(%source, path = %temporary_path.display(), "pack committed with staging directory sync pending");
        return Ok(ImmutablePublication {
            ownership,
            cleanup_state: ProjectionPackCleanupState::PendingStartupRepair,
        });
    }
    if let Err(source) = sync_parent_directory(&temporary_path) {
        tracing::warn!(%source, path = %temporary_path.display(), "pack committed with staging directory sync pending");
        return Ok(ImmutablePublication {
            ownership,
            cleanup_state: ProjectionPackCleanupState::PendingStartupRepair,
        });
    }
    Ok(ImmutablePublication {
        ownership,
        cleanup_state: ProjectionPackCleanupState::Complete,
    })
}

fn sync_parent_directory(path: &Path) -> Result<(), ProjectionPackError> {
    let parent = path.parent().ok_or_else(|| ProjectionPackError::Publish {
        path: path.to_path_buf(),
        source: io::Error::other("immutable pack has no parent directory"),
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ProjectionPackError::Publish {
            path: path.to_path_buf(),
            source,
        })
}
