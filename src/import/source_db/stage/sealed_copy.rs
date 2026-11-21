// SPDX-License-Identifier: AGPL-3.0-only

//! A private copy of a sealed physical stage (M03-T22): the raw stage an
//! import published is kept, and a later handback plans a copy of it, so a
//! window's data reaches 1.4 clients without another import.

use std::{
    ffi::OsString,
    os::{
        fd::{AsFd, OwnedFd},
        unix::fs::FileExt,
    },
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags};
use uuid::Uuid;

use super::{
    PrivateDirectory, STAGE_FILE_EXTENSION, STAGING_DIRECTORY, SourceDbStage, SourceDbStageError,
    SourceDbStageFormat, SourceDbStageLimits, SourceDbStageState, StageFileIdentity, available_bytes, configure_writable_connection,
    create_private_stage_file, ensure_private_child_directory, ensure_private_directory,
    read_stage_format, verify_stage_path_identity,
};

const COPY_BLOCK_BYTES: usize = 1024 * 1024;

#[cfg(test)]
thread_local! {
    /// Runs once on the next copy's file after its bytes are written and
    /// before SQLite opens it (M08-T19 tests: damage only the copy).
    pub(crate) static AFTER_SEALED_COPY: std::cell::RefCell<Option<Box<dyn FnOnce(&Path)>>> =
        const { std::cell::RefCell::new(None) };
}

impl SourceDbStage {
    /// Copies this sealed physical stage into a new private stage under
    /// `<imports_dir>/.staging`, byte for byte, and opens the copy writable
    /// and still sealed, so [`SourceDbStage::edit_sealed`] can plan it. The
    /// copy needs the stage's own byte limit free on the target filesystem.
    /// The copy's accounting is checked here, not its pages: a publication
    /// checks the database it reads after its last write (M08-T19).
    pub(crate) fn copy_sealed_into(
        &self,
        imports_dir: impl AsRef<Path>,
    ) -> Result<Self, SourceDbStageError> {
        let format = self.format()?;
        if format != SourceDbStageFormat::PhysicalV3 {
            return Err(SourceDbStageError::TableFormatMismatch {
                format: format.as_str(),
                table: "stage_rows",
            });
        }
        let stats = self.stats()?;
        if stats.state != SourceDbStageState::Sealed {
            return Err(SourceDbStageError::StageNotSealed);
        }
        let limits = stats.limits;
        let imports_dir = ensure_private_directory(imports_dir.as_ref())?;
        let staging_dir = ensure_private_child_directory(&imports_dir, STAGING_DIRECTORY)?;
        let required_free_bytes = limits
            .max_stage_bytes
            .checked_add(limits.minimum_free_bytes)
            .ok_or(SourceDbStageError::StageCapacityOverflow)?;
        let available_free_bytes = available_bytes(&staging_dir.path)?;
        if available_free_bytes < required_free_bytes {
            return Err(SourceDbStageError::InsufficientFreeSpace {
                required: required_free_bytes,
                available: available_free_bytes,
            });
        }
        let file_name = OsString::from(format!("{}.{}", Uuid::new_v4(), STAGE_FILE_EXTENSION));
        let path = staging_dir.path.join(&file_name);
        let (file_descriptor, file_identity) =
            create_private_stage_file(&staging_dir, &file_name, &path)?;
        let copied = (|| -> Result<(), SourceDbStageError> {
            let source = std::fs::File::from(
                self.file_descriptor
                    .as_fd()
                    .try_clone_to_owned()
                    .map_err(SourceDbStageError::Durability)?,
            );
            let target = std::fs::File::from(
                file_descriptor
                    .as_fd()
                    .try_clone_to_owned()
                    .map_err(SourceDbStageError::Durability)?,
            );
            let mut buffer = vec![0_u8; COPY_BLOCK_BYTES];
            let mut offset = 0_u64;
            loop {
                let read = source
                    .read_at(&mut buffer, offset)
                    .map_err(SourceDbStageError::Durability)?;
                if read == 0 {
                    break;
                }
                target
                    .write_all_at(&buffer[..read], offset)
                    .map_err(SourceDbStageError::Durability)?;
                offset += read as u64;
                if offset > limits.max_stage_bytes {
                    return Err(SourceDbStageError::DatabaseByteLimitExceeded {
                        maximum: limits.max_stage_bytes,
                    });
                }
            }
            target.sync_all().map_err(SourceDbStageError::Durability)
        })();
        if let Err(error) = copied {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        #[cfg(test)]
        if let Some(damage) = AFTER_SEALED_COPY.with(|hook| hook.borrow_mut().take()) {
            damage(&path);
        }
        Self::open_private_sealed_copy(
            staging_dir,
            file_name,
            path,
            file_descriptor,
            file_identity,
            limits,
        )
    }

    /// Opens a private sealed copy just written under a `.staging`
    /// directory (a copy of a kept stage, or a decoded kept archive) as a
    /// writable sealed stage, checking its state and accounting, not its
    /// pages. A copy that fails is removed.
    pub(super) fn open_private_sealed_copy(
        staging_dir: PrivateDirectory,
        file_name: OsString,
        path: PathBuf,
        file_descriptor: OwnedFd,
        file_identity: StageFileIdentity,
        limits: SourceDbStageLimits,
    ) -> Result<Self, SourceDbStageError> {
        let opened = (|| -> Result<(Connection, SourceDbStageFormat), SourceDbStageError> {
            let connection = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?;
            verify_stage_path_identity(&staging_dir, &file_name, &path, file_identity)?;
            configure_writable_connection(&connection, limits)?;
            let format = read_stage_format(&connection)?;
            Ok((connection, format))
        })();
        let (connection, format) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                return Err(error);
            }
        };
        let copy = Self {
            path,
            connection,
            writable: true,
            format,
            file_identity,
            directory: staging_dir,
            file_name,
            file_descriptor,
        };
        let checked = (|| -> Result<(), SourceDbStageError> {
            let copied = copy.stats()?;
            if copied.state != SourceDbStageState::Sealed {
                return Err(SourceDbStageError::StageNotSealed);
            }
            copy.verify_accounting(copied)
        })();
        match checked {
            Ok(()) => Ok(copy),
            Err(error) => {
                let _ = copy.discard();
                Err(error)
            }
        }
    }
}
