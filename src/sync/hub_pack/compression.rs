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

#[cfg(test)]
thread_local! {
    /// Test only (#52): consulted at every open of a pack by the physical
    /// purpose verifier; `Some` bytes stand in for the file's content at that
    /// open, so a test can count the opens and change what one of them reads.
    pub(crate) static PURPOSE_OPEN_VIEW: std::cell::RefCell<
        Option<Box<dyn FnMut(&Path) -> Option<Vec<u8>>>>,
    > = const { std::cell::RefCell::new(None) };
}

/// Feeds every compressed byte the verifier reads into a streaming decoder as
/// well, so the bytes that are hashed, size-checked and header-checked are the
/// very bytes decoded into the staged SQLite, in the same read (#52).
struct DecodingTee<R, W: Write> {
    inner: R,
    decoder: zstd::stream::write::Decoder<'static, W>,
}

impl<R: Read, W: Write> Read for DecodingTee<R, W> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.decoder.write_all(&buffer[..read])?;
        Ok(read)
    }
}

/// A file writer that refuses output beyond the declared decoded length.
struct BoundedWriter {
    file: File,
    written: u64,
    limit: u64,
}

impl Write for BoundedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(buffer.len()).map_err(io::Error::other)?;
        if self.written.checked_add(length).is_none_or(|total| total > self.limit) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "physical publication pack decodes beyond its declared length",
            ));
        }
        let written = self.file.write(buffer)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn open_purpose_source(path: &Path) -> Result<Box<dyn Read>, ProjectionPackError> {
    #[cfg(test)]
    if let Some(view) =
        PURPOSE_OPEN_VIEW.with(|hook| hook.borrow_mut().as_mut().and_then(|view| view(path)))
    {
        return Ok(Box::new(io::Cursor::new(view)));
    }
    let file = File::open(path).map_err(|source| ProjectionPackError::OpenCompressed {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Box::new(file))
}

fn verify_physical_publication_file_2_2(
    metadata: &TransportPack,
    path: &Path,
    manifest: &SyncManifest,
    binding: &ProjectionBinding,
) -> Result<(), ProjectionPackError> {
    let limits = ProtocolLimits::hub_sync_v1_1_3_schema_2_2();
    // Validate the declared sizes against the receiver profile before any
    // byte is decoded, so the decoded-length bound below is the profile's too.
    metadata
        .validate(limits)
        .map_err(ProjectionPackError::Protocol)?;
    let staging_dir = path
        .parent()
        .and_then(Path::parent)
        .map(|packs| packs.join(".staging"))
        .ok_or_else(|| invalid("physical publication pack path has no staging directory"))?;
    ensure_private_staging_directory(&staging_dir)?;
    let sqlite = StagedFile::create(&staging_dir, "physical-admission.sqlite")?;
    let output = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(sqlite.path())
        .map_err(|source| ProjectionPackError::CreateTemporary {
            path: sqlite.path().to_path_buf(),
            source,
        })?;
    let decoder = zstd::stream::write::Decoder::new(BoundedWriter {
        file: output,
        written: 0,
        limit: metadata.uncompressed_bytes,
    })
    .map_err(ProjectionPackError::Decompress)?;
    // One open and one read: the protocol verifier authenticates the digest,
    // the compressed and decoded sizes and the SQLite header over the same
    // bytes the tee decodes, so the identity check below inspects only bytes
    // covered by that attestation.
    let mut tee = DecodingTee {
        inner: open_purpose_source(path)?,
        decoder,
    };
    metadata
        .verify_reader(&mut tee, limits)
        .map_err(ProjectionPackError::Protocol)?;
    tee.decoder
        .flush()
        .map_err(ProjectionPackError::Decompress)?;
    let output = tee.decoder.into_inner();
    if output.written != metadata.uncompressed_bytes {
        return Err(invalid("physical publication pack decoded length is invalid"));
    }
    output
        .file
        .sync_all()
        .map_err(ProjectionPackError::SyncCompressed)?;
    drop(output);
    verify_projection_sqlite_2_2_publication_identity(sqlite.path(), metadata, manifest, binding)
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
    crate::platform::file_attempts::note(destination_path);
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

pub(crate) fn available_bytes(path: &Path) -> Result<u64, ProjectionPackError> {
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
    crate::platform::file_attempts::note(&temporary_path);
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
    crate::platform::file_attempts::note(final_path);
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
    let synced = crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::PackFinalDirectoryFsync,
    )
    .map_err(ProjectionPackError::Durability)
    .and_then(|()| sync_parent_directory(final_path));
    if let Err(error) = synced {
        // A content object this write created is named in the error, so the caller
        // can remove it under its publication gate (M04-T08); a reused one is kept.
        return Err(match ownership {
            ProjectionPackOwnership::Created => ProjectionPackError::InstalledThenFailed {
                path: final_path.to_path_buf(),
                sha256: metadata.sha256,
                source: Box::new(error),
            },
            ProjectionPackOwnership::ReusedExisting => error,
        });
    }
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
    crate::platform::file_attempts::note(&temporary_path);
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
    // A sync leaves the file image as it was and is counted in tests all the same (B2 fix
    // round, P3b fix 5, R-P3bF5-3).
    crate::platform::file_attempts::note(parent);
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ProjectionPackError::Publish {
            path: path.to_path_buf(),
            source,
        })
}
