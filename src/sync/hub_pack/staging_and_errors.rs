// SPDX-License-Identifier: AGPL-3.0-only

struct StagedFile {
    path: PathBuf,
    cleanup_on_drop: bool,
}

impl StagedFile {
    fn create(directory: &Path, extension: &str) -> Result<Self, ProjectionPackError> {
        for _ in 0..32 {
            let path = directory.join(format!("{}.{}.tmp", Uuid::new_v4(), extension));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => {
                    drop(file);
                    return Ok(Self {
                        path,
                        cleanup_on_drop: true,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(ProjectionPackError::CreateTemporary { path, source }),
            }
        }
        Err(ProjectionPackError::CreateTemporary {
            path: directory.join(format!("exhausted.{extension}.tmp")),
            source: io::Error::new(io::ErrorKind::AlreadyExists, "temporary name collision"),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn retain_for_repair(&mut self) {
        self.cleanup_on_drop = false;
    }

    fn mark_removed(&mut self) {
        self.cleanup_on_drop = false;
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    bytes_written: u64,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes_written: 0,
        }
    }

    fn finish(self) -> (W, Sha256Digest, u64) {
        (
            self.inner,
            Sha256Digest::from_bytes(self.hasher.finalize().into()),
            self.bytes_written,
        )
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.bytes_written += u64::try_from(written).expect("usize fits into u64");
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Debug, Error)]
pub enum ProjectionPackError {
    #[error("invalid Hub projection pack: {0}")]
    Invalid(String),
    #[error("projection pack exceeds the configured row limit")]
    TooManyRows,
    #[error("projection snapshot has too many chunks")]
    TooManyChunks,
    #[error("projection snapshot totals overflow")]
    ManifestTotalsOverflow,
    #[error("projection pack capacity calculation overflowed")]
    CapacityOverflow,
    #[error("could not inspect free space for projection packs at {path}: {source}")]
    FilesystemSpace {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error(
        "projection full snapshot needs {required} free bytes but only {available} are available"
    )]
    InsufficientFreeSpace { required: u64, available: u64 },
    #[error("cannot create pack directory {path}: {source}")]
    CreateDirectory { path: PathBuf, source: io::Error },
    #[error("cannot inspect projection pack staging path {path}: {source}")]
    InspectStaging { path: PathBuf, source: io::Error },
    #[error("projection pack staging path is unsafe: {0}")]
    UnsafeStaging(PathBuf),
    #[error("cannot protect projection pack staging directory {path}: {source}")]
    ProtectStaging { path: PathBuf, source: io::Error },
    #[error("cannot clean projection pack staging path {path}: {source}")]
    CleanupStaging { path: PathBuf, source: io::Error },
    #[error("cannot create temporary projection pack {path}: {source}")]
    CreateTemporary { path: PathBuf, source: io::Error },
    #[error("cannot inspect temporary projection pack {path}: {source}")]
    Metadata { path: PathBuf, source: io::Error },
    #[error("cannot open projection SQLite pack: {0}")]
    OpenSqlite(rusqlite::Error),
    #[error("cannot configure projection SQLite pack: {0}")]
    ConfigureSqlite(rusqlite::Error),
    #[error("cannot create projection SQLite schema: {0}")]
    CreateSchema(rusqlite::Error),
    #[error("cannot begin projection SQLite transaction: {0}")]
    BeginTransaction(rusqlite::Error),
    #[error("cannot prepare projection insert: {0}")]
    Prepare(rusqlite::Error),
    #[error("cannot insert projection row: {0}")]
    Insert(rusqlite::Error),
    #[error("cannot commit projection SQLite transaction: {0}")]
    Commit(rusqlite::Error),
    #[error("cannot finalise projection SQLite pack: {0}")]
    FinalizeSqlite(rusqlite::Error),
    #[error("projection SQLite integrity check failed to run: {0}")]
    IntegrityCheck(rusqlite::Error),
    #[error("projection SQLite integrity check failed")]
    IntegrityFailure,
    #[error("cannot read projection SQLite source {path}: {source}")]
    ReadSource { path: PathBuf, source: io::Error },
    #[error("cannot create compressed projection pack {path}: {source}")]
    CreateCompressed { path: PathBuf, source: io::Error },
    #[error("cannot compress projection pack: {0}")]
    Compress(io::Error),
    #[error("cannot synchronise compressed projection pack: {0}")]
    SyncCompressed(io::Error),
    #[error("projection durability checkpoint failed: {0}")]
    Durability(io::Error),
    #[error("cannot open compressed projection pack {path}: {source}")]
    OpenCompressed { path: PathBuf, source: io::Error },
    #[error("cannot publish immutable projection pack {path}: {source}")]
    Publish { path: PathBuf, source: io::Error },
    #[error("projection protocol validation failed: {0}")]
    Protocol(#[from] ProtocolError),
}
