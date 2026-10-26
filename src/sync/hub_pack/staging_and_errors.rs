// SPDX-License-Identifier: AGPL-3.0-only

struct StagedFile {
    path: PathBuf,
    cleanup_on_drop: bool,
}

impl StagedFile {
    fn create(directory: &Path, extension: &str) -> Result<Self, ProjectionPackError> {
        for _ in 0..32 {
            let path = directory.join(format!("{}.{}.tmp", Uuid::new_v4(), extension));
            crate::platform::file_attempts::note(&path);
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
            crate::platform::file_attempts::note(&self.path);
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
    #[error("cannot decompress projection pack: {0}")]
    Decompress(io::Error),
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

#[cfg(test)]
mod n1_src_sync_hub_pack_staging_and_errors_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free, example};

    impl ErrorExample for ProjectionPackError {
        fn error_example() -> Self {
            ProjectionPackError::TooManyRows
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_projection_pack_error(error: ProjectionPackError) {
            match &error {
                ProjectionPackError::Invalid(..) => (),
                ProjectionPackError::TooManyRows => (),
                ProjectionPackError::TooManyChunks => (),
                ProjectionPackError::ManifestTotalsOverflow => (),
                ProjectionPackError::CapacityOverflow => (),
                ProjectionPackError::FilesystemSpace { .. } => (),
                ProjectionPackError::InsufficientFreeSpace { .. } => (),
                ProjectionPackError::CreateDirectory { .. } => (),
                ProjectionPackError::InspectStaging { .. } => (),
                ProjectionPackError::UnsafeStaging(..) => (),
                ProjectionPackError::ProtectStaging { .. } => (),
                ProjectionPackError::CleanupStaging { .. } => (),
                ProjectionPackError::CreateTemporary { .. } => (),
                ProjectionPackError::Metadata { .. } => (),
                ProjectionPackError::OpenSqlite(..) => (),
                ProjectionPackError::ConfigureSqlite(..) => (),
                ProjectionPackError::CreateSchema(..) => (),
                ProjectionPackError::BeginTransaction(..) => (),
                ProjectionPackError::Prepare(..) => (),
                ProjectionPackError::Insert(..) => (),
                ProjectionPackError::Commit(..) => (),
                ProjectionPackError::FinalizeSqlite(..) => (),
                ProjectionPackError::IntegrityCheck(..) => (),
                ProjectionPackError::IntegrityFailure => (),
                ProjectionPackError::ReadSource { .. } => (),
                ProjectionPackError::CreateCompressed { .. } => (),
                ProjectionPackError::Compress(..) => (),
                ProjectionPackError::Decompress(..) => (),
                ProjectionPackError::SyncCompressed(..) => (),
                ProjectionPackError::Durability(..) => (),
                ProjectionPackError::OpenCompressed { .. } => (),
                ProjectionPackError::Publish { .. } => (),
                ProjectionPackError::Protocol(..) => (),
            }
            assert_name_free(&error);
        }
        check_projection_pack_error(ProjectionPackError::Invalid(example()));
        check_projection_pack_error(ProjectionPackError::TooManyRows);
        check_projection_pack_error(ProjectionPackError::TooManyChunks);
        check_projection_pack_error(ProjectionPackError::ManifestTotalsOverflow);
        check_projection_pack_error(ProjectionPackError::CapacityOverflow);
        check_projection_pack_error(ProjectionPackError::FilesystemSpace {
            path: example(),
            source: example(),
        });
        check_projection_pack_error(ProjectionPackError::InsufficientFreeSpace {
            required: example(),
            available: example(),
        });
        check_projection_pack_error(ProjectionPackError::CreateDirectory {
            path: example(),
            source: example(),
        });
        check_projection_pack_error(ProjectionPackError::InspectStaging {
            path: example(),
            source: example(),
        });
        check_projection_pack_error(ProjectionPackError::UnsafeStaging(example()));
        check_projection_pack_error(ProjectionPackError::ProtectStaging {
            path: example(),
            source: example(),
        });
        check_projection_pack_error(ProjectionPackError::CleanupStaging {
            path: example(),
            source: example(),
        });
        check_projection_pack_error(ProjectionPackError::CreateTemporary {
            path: example(),
            source: example(),
        });
        check_projection_pack_error(ProjectionPackError::Metadata {
            path: example(),
            source: example(),
        });
        check_projection_pack_error(ProjectionPackError::OpenSqlite(example()));
        check_projection_pack_error(ProjectionPackError::ConfigureSqlite(example()));
        check_projection_pack_error(ProjectionPackError::CreateSchema(example()));
        check_projection_pack_error(ProjectionPackError::BeginTransaction(example()));
        check_projection_pack_error(ProjectionPackError::Prepare(example()));
        check_projection_pack_error(ProjectionPackError::Insert(example()));
        check_projection_pack_error(ProjectionPackError::Commit(example()));
        check_projection_pack_error(ProjectionPackError::FinalizeSqlite(example()));
        check_projection_pack_error(ProjectionPackError::IntegrityCheck(example()));
        check_projection_pack_error(ProjectionPackError::IntegrityFailure);
        check_projection_pack_error(ProjectionPackError::ReadSource {
            path: example(),
            source: example(),
        });
        check_projection_pack_error(ProjectionPackError::CreateCompressed {
            path: example(),
            source: example(),
        });
        check_projection_pack_error(ProjectionPackError::Compress(example()));
        check_projection_pack_error(ProjectionPackError::Decompress(example()));
        check_projection_pack_error(ProjectionPackError::SyncCompressed(example()));
        check_projection_pack_error(ProjectionPackError::Durability(example()));
        check_projection_pack_error(ProjectionPackError::OpenCompressed {
            path: example(),
            source: example(),
        });
        check_projection_pack_error(ProjectionPackError::Publish {
            path: example(),
            source: example(),
        });
        check_projection_pack_error(ProjectionPackError::Protocol(example()));
    }
}
