//! Private deterministic durability checkpoints used only by unit tests.
//!
//! Production builds retain no way to arm a checkpoint. The no-op `check`
//! calls keep the write ordering visible beside the I/O they protect.

use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurabilityFaultPoint {
    PackSqliteCommit,
    PackCompressedWrite,
    PackCompressedFsync,
    PackFinalInstall,
    PackFinalDirectoryFsync,
    PackStagingUnlink,
    PackStagingDirectoryFsync,
    Schema22NoOpWrite,
    Schema22NoOpFsync,
    Schema22NoOpRename,
    Schema22NoOpDirectoryFsync,
    CatalogueBeforeCommit,
    CatalogueAfterCommit,
}

impl DurabilityFaultPoint {
    const fn label(self) -> &'static str {
        match self {
            Self::PackSqliteCommit => "pack_sqlite_commit",
            Self::PackCompressedWrite => "pack_compressed_write",
            Self::PackCompressedFsync => "pack_compressed_fsync",
            Self::PackFinalInstall => "pack_final_install",
            Self::PackFinalDirectoryFsync => "pack_final_directory_fsync",
            Self::PackStagingUnlink => "pack_staging_unlink",
            Self::PackStagingDirectoryFsync => "pack_staging_directory_fsync",
            Self::Schema22NoOpWrite => "schema_22_noop_write",
            Self::Schema22NoOpFsync => "schema_22_noop_fsync",
            Self::Schema22NoOpRename => "schema_22_noop_rename",
            Self::Schema22NoOpDirectoryFsync => "schema_22_noop_directory_fsync",
            Self::CatalogueBeforeCommit => "catalogue_before_commit",
            Self::CatalogueAfterCommit => "catalogue_after_commit",
        }
    }
}

#[cfg(test)]
thread_local! {
    static ARMED: std::cell::Cell<Option<DurabilityFaultPoint>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn check(point: DurabilityFaultPoint) -> io::Result<()> {
    #[cfg(test)]
    {
        if ARMED.with(|armed| {
            if armed.get() == Some(point) {
                armed.set(None);
                true
            } else {
                false
            }
        }) {
            return Err(io::Error::other(format!(
                "injected durability fault at {}",
                point.label()
            )));
        }
    }
    #[cfg(not(test))]
    let _ = point.label();
    Ok(())
}

#[cfg(test)]
pub(crate) struct FaultGuard {
    prior: Option<DurabilityFaultPoint>,
}

#[cfg(test)]
impl Drop for FaultGuard {
    fn drop(&mut self) {
        ARMED.with(|armed| armed.set(self.prior));
    }
}

#[cfg(test)]
pub(crate) fn inject(point: DurabilityFaultPoint) -> FaultGuard {
    let prior = ARMED.with(|armed| armed.replace(Some(point)));
    FaultGuard { prior }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fault_is_thread_local_one_shot_and_redacted() {
        let _guard = inject(DurabilityFaultPoint::PackCompressedFsync);
        let error = check(DurabilityFaultPoint::PackCompressedFsync).expect_err("armed fault");
        assert_eq!(
            error.to_string(),
            "injected durability fault at pack_compressed_fsync"
        );
        check(DurabilityFaultPoint::PackCompressedFsync).expect("fault fires once");
    }
}
