// SPDX-License-Identifier: AGPL-3.0-only

fn validate_transfer_database(
    connection: &Connection,
    schema: &str,
    expected: TeslaMateProjectionStateStats,
    selected_car_id: i64,
) -> Result<(), TeslaMateProjectionStateError> {
    debug_assert!(matches!(
        schema,
        "main" | TESLAMATE_PROJECTION_STATE_ATTACHMENT_SCHEMA
    ));
    validate_transfer_schema(connection, schema)?;

    let integrity: String = connection
        .query_row(&format!("PRAGMA {schema}.integrity_check"), [], |row| {
            row.get(0)
        })
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if integrity != "ok" {
        return Err(TeslaMateProjectionStateError::IntegrityCheckFailed(
            integrity,
        ));
    }
    let foreign_key_violation = connection
        .prepare(&format!("PRAGMA {schema}.foreign_key_check"))
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .exists([])
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if foreign_key_violation {
        return Err(TeslaMateProjectionStateError::ForeignKeyCheckFailed);
    }

    let current_rows = qualified_transfer_table(schema, "current_rows");
    let changed_rows = qualified_transfer_table(schema, "changed_rows");
    let (row_count, changed_row_count, changed_payload_bytes): (i64, i64, i64) = connection
        .query_row(
            &format!(
                "SELECT \
                    (SELECT COUNT(*) FROM {current_rows}), \
                    (SELECT COUNT(*) FROM {changed_rows}), \
                    (SELECT COALESCE(SUM(payload_bytes), 0) FROM {changed_rows})"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    let actual = TeslaMateProjectionStateStats {
        row_count: u64::try_from(row_count)
            .map_err(|_| TeslaMateProjectionStateError::InvalidStoredAccounting)?,
        changed_row_count: u64::try_from(changed_row_count)
            .map_err(|_| TeslaMateProjectionStateError::InvalidStoredAccounting)?,
        changed_payload_bytes: u64::try_from(changed_payload_bytes)
            .map_err(|_| TeslaMateProjectionStateError::InvalidStoredAccounting)?,
        sealed: true,
    };
    if actual != expected {
        return Err(TeslaMateProjectionStateError::TransferAccountingMismatch);
    }

    let invalid_current: bool = connection
        .query_row(
            &format!(
                "SELECT EXISTS(
                    SELECT 1 FROM {current_rows}
                     WHERE typeof(entity_ordinal) <> 'integer'
                        OR entity_ordinal NOT BETWEEN 0 AND 6
                        OR typeof(entity_id) <> 'integer'
                        OR entity_id <= 0
                        OR typeof(car_id) <> 'integer'
                        OR car_id <> ?1
                        OR typeof(projection_sha256) <> 'blob'
                        OR length(projection_sha256) <> 32
                        OR (entity_ordinal = 0 AND entity_id <> ?1)
                )"
            ),
            params![selected_car_id],
            |row| row.get(0),
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if invalid_current {
        return Err(TeslaMateProjectionStateError::TransferRowContractMismatch);
    }
    let cars: i64 = connection
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM {current_rows}
                  WHERE entity_ordinal = 0 AND entity_id = ?1 AND car_id = ?1"
            ),
            params![selected_car_id],
            |row| row.get(0),
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if cars != 1 {
        return Err(TeslaMateProjectionStateError::TransferCarContractMismatch);
    }

    let invalid_changed: bool = connection
        .query_row(
            &format!(
                "SELECT EXISTS(
                    SELECT 1
                      FROM {changed_rows} AS changed
                 LEFT JOIN {current_rows} AS current
                        ON current.entity_ordinal = changed.entity_ordinal
                       AND current.entity_id = changed.entity_id
                     WHERE typeof(changed.entity_ordinal) <> 'integer'
                        OR changed.entity_ordinal NOT BETWEEN 0 AND 6
                        OR typeof(changed.entity_id) <> 'integer'
                        OR changed.entity_id <= 0
                        OR typeof(changed.car_id) <> 'integer'
                        OR changed.car_id <> ?1
                        OR typeof(changed.projection_sha256) <> 'blob'
                        OR length(changed.projection_sha256) <> 32
                        OR typeof(changed.payload_json) <> 'text'
                        OR json_valid(changed.payload_json) <> 1
                        OR typeof(changed.payload_bytes) <> 'integer'
                        OR changed.payload_bytes < 0
                        OR changed.payload_bytes <> length(CAST(changed.payload_json AS BLOB))
                        OR current.entity_id IS NULL
                        OR current.car_id <> changed.car_id
                        OR current.projection_sha256 <> changed.projection_sha256
                )"
            ),
            params![selected_car_id],
            |row| row.get(0),
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if invalid_changed {
        return Err(TeslaMateProjectionStateError::TransferRowContractMismatch);
    }
    Ok(())
}

fn validate_transfer_schema(
    connection: &Connection,
    schema: &str,
) -> Result<(), TeslaMateProjectionStateError> {
    let objects = connection
        .prepare(&format!(
            "SELECT type, name, tbl_name, sql
               FROM {schema}.sqlite_schema
              WHERE name NOT LIKE 'sqlite_%'
              ORDER BY type ASC, name ASC"
        ))
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if objects.len() != 2
        || objects.iter().any(|(kind, name, table, sql)| {
            kind != "table"
                || table != name
                || !matches!(name.as_str(), "changed_rows" | "current_rows")
                || !sql
                    .as_deref()
                    .is_some_and(|sql| sql.contains("STRICT") && sql.contains("WITHOUT ROWID"))
        })
    {
        return Err(TeslaMateProjectionStateError::TransferSchemaMismatch);
    }
    validate_transfer_table_columns(
        connection,
        schema,
        "current_rows",
        &[
            ("entity_ordinal", "INTEGER", 1_i64),
            ("entity_id", "INTEGER", 2_i64),
            ("car_id", "INTEGER", 0_i64),
            ("projection_sha256", "BLOB", 0_i64),
        ],
    )?;
    validate_transfer_table_columns(
        connection,
        schema,
        "changed_rows",
        &[
            ("entity_ordinal", "INTEGER", 1_i64),
            ("entity_id", "INTEGER", 2_i64),
            ("car_id", "INTEGER", 0_i64),
            ("projection_sha256", "BLOB", 0_i64),
            ("payload_json", "TEXT", 0_i64),
            ("payload_bytes", "INTEGER", 0_i64),
        ],
    )?;
    let foreign_keys = connection
        .prepare(&format!("PRAGMA {schema}.foreign_key_list(changed_rows)"))
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    let expected_foreign_keys = HashSet::from([
        (
            "current_rows".to_owned(),
            "entity_ordinal".to_owned(),
            "entity_ordinal".to_owned(),
            "CASCADE".to_owned(),
        ),
        (
            "current_rows".to_owned(),
            "entity_id".to_owned(),
            "entity_id".to_owned(),
            "CASCADE".to_owned(),
        ),
        (
            "current_rows".to_owned(),
            "car_id".to_owned(),
            "car_id".to_owned(),
            "CASCADE".to_owned(),
        ),
        (
            "current_rows".to_owned(),
            "projection_sha256".to_owned(),
            "projection_sha256".to_owned(),
            "CASCADE".to_owned(),
        ),
    ]);
    if foreign_keys.len() != expected_foreign_keys.len()
        || foreign_keys.into_iter().collect::<HashSet<_>>() != expected_foreign_keys
    {
        return Err(TeslaMateProjectionStateError::TransferSchemaMismatch);
    }
    Ok(())
}

fn validate_transfer_table_columns(
    connection: &Connection,
    schema: &str,
    table: &str,
    expected: &[(&str, &str, i64)],
) -> Result<(), TeslaMateProjectionStateError> {
    let columns = connection
        .prepare(&format!("PRAGMA {schema}.table_info({table})"))
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if columns.len() != expected.len()
        || columns.iter().zip(expected).enumerate().any(
            |(
                index,
                (
                    (cid, name, ty, not_null, default, primary_key),
                    (expected_name, expected_ty, expected_primary_key),
                ),
            )| {
                *cid != i64::try_from(index).expect("column index fits i64")
                    || name != expected_name
                    || ty != expected_ty
                    || *not_null != 1
                    || default.is_some()
                    || primary_key != expected_primary_key
            },
        )
    {
        return Err(TeslaMateProjectionStateError::TransferSchemaMismatch);
    }
    Ok(())
}

fn transfer_semantic_digest(
    connection: &Connection,
    schema: &str,
) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
    let mut digest = Sha256::new();
    digest.update(TRANSFER_DIGEST_DOMAIN);
    digest.update([0]);
    for (tag, table, columns) in [
        (
            b'c',
            "current_rows",
            "entity_ordinal, entity_id, car_id, projection_sha256",
        ),
        (
            b'h',
            "changed_rows",
            "entity_ordinal, entity_id, car_id, projection_sha256, payload_json, payload_bytes",
        ),
    ] {
        digest.update([tag]);
        let table = qualified_transfer_table(schema, table);
        let mut statement = connection
            .prepare(&format!(
                "SELECT {columns} FROM {table} ORDER BY entity_ordinal, entity_id"
            ))
            .map_err(TeslaMateProjectionStateError::Sqlite)?;
        let mut rows = statement
            .query([])
            .map_err(TeslaMateProjectionStateError::Sqlite)?;
        while let Some(row) = rows.next().map_err(TeslaMateProjectionStateError::Sqlite)? {
            let entity_ordinal = row
                .get::<_, i64>(0)
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            let entity_id = row
                .get::<_, i64>(1)
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            let car_id = row
                .get::<_, i64>(2)
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            let row_digest = row
                .get::<_, Vec<u8>>(3)
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            digest.update(entity_ordinal.to_be_bytes());
            digest.update(entity_id.to_be_bytes());
            digest.update(car_id.to_be_bytes());
            digest.update(
                u64::try_from(row_digest.len())
                    .expect("digest length fits u64")
                    .to_be_bytes(),
            );
            digest.update(row_digest);
            if tag == b'h' {
                let payload = row
                    .get::<_, String>(4)
                    .map_err(TeslaMateProjectionStateError::Sqlite)?;
                let payload_bytes = row
                    .get::<_, i64>(5)
                    .map_err(TeslaMateProjectionStateError::Sqlite)?;
                digest.update(payload_bytes.to_be_bytes());
                digest.update(
                    u64::try_from(payload.len())
                        .expect("payload length fits u64")
                        .to_be_bytes(),
                );
                digest.update(payload.as_bytes());
            }
        }
    }
    Ok(Sha256Digest::from_bytes(digest.finalize().into()))
}

fn qualified_transfer_table(schema: &str, table: &str) -> String {
    debug_assert!(matches!(
        schema,
        "main" | TESLAMATE_PROJECTION_STATE_ATTACHMENT_SCHEMA
    ));
    debug_assert!(matches!(table, "current_rows" | "changed_rows"));
    format!("{schema}.{table}")
}

fn is_unique_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == ErrorCode::ConstraintViolation
    )
}

fn ensure_existing_directory(path: &Path) -> Result<(), TeslaMateProjectionStateError> {
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
    if !metadata.is_dir() {
        return Err(TeslaMateProjectionStateError::ExpectedDirectory(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
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
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|source| {
                TeslaMateProjectionStateError::CreateDirectory {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
        }
        Err(source) => {
            return Err(TeslaMateProjectionStateError::InspectPath {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    set_private_directory_permissions(path)
}

fn ensure_private_file(path: &Path) -> Result<(), TeslaMateProjectionStateError> {
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
    set_private_file_permissions(path)
}

fn available_bytes(path: &Path) -> Result<u64, TeslaMateProjectionStateError> {
    let stats = rustix::fs::statvfs(path).map_err(|source| {
        TeslaMateProjectionStateError::FilesystemSpace {
            path: path.to_path_buf(),
            source,
        }
    })?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or(TeslaMateProjectionStateError::StateCapacityOverflow)
}

fn write_batch_required_free_bytes(
    minimum_free_bytes: u64,
) -> Result<u64, TeslaMateProjectionStateError> {
    minimum_free_bytes
        .checked_add(WRITE_BATCH_HEADROOM_BYTES)
        .ok_or(TeslaMateProjectionStateError::StateCapacityOverflow)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        TeslaMateProjectionStateError::SetPermissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        TeslaMateProjectionStateError::SetPermissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum TeslaMateProjectionStateError {
    #[error("projection-state maximum rows must be positive")]
    InvalidMaximumRows,
    #[error("projection-state maximum database bytes must be at least {minimum}")]
    InvalidMaximumStateBytes { minimum: u64 },
    #[error("projection-state changed payload budget must be positive")]
    InvalidMaximumChangedPayloadBytes,
    #[error("projection-state changed-row payload cap must be positive and fit SQLite accounting")]
    InvalidChangedRowPayloadLimit,
    #[error("projection-state changed payload budget exceeds total state capacity")]
    ChangedPayloadBudgetExceedsStateCapacity,
    #[error("projection-state capacity calculation overflowed")]
    StateCapacityOverflow,
    #[error("could not create private projection-state directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not create private projection-state file {path}: {source}")]
    CreateFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not inspect projection-state path {path}: {source}")]
    InspectPath {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not set private permissions on {path}: {source}")]
    SetPermissions {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("projection-state path may not be a symlink: {0}")]
    SymlinkPath(PathBuf),
    #[error("expected projection-state directory at {0}")]
    ExpectedDirectory(PathBuf),
    #[error("expected projection-state file at {0}")]
    ExpectedFile(PathBuf),
    #[error("projection-state transfer path is not a canonical private spool: {0}")]
    InvalidTransferPath(PathBuf),
    #[error("projection-state transfer path is not private to the current Hub user: {0}")]
    TransferPathNotPrivate(PathBuf),
    #[error("direct-import projection-state run id must be a non-nil UUID")]
    InvalidImportGenerationRunId,
    #[error("direct-import finalization requires a run-bound projection-state spool")]
    ImportGenerationTransferRequired,
    #[error("direct-import projection-state spool belongs to run {expected}, not {actual}")]
    ImportGenerationRunMismatch { expected: Uuid, actual: Uuid },
    #[error("direct-import projection-state transfer path is outside its owned v1 namespace: {0}")]
    InvalidImportGenerationTransferPath(PathBuf),
    #[error(
        "direct-import projection-state namespace changed after creation (expected {expected}, got {actual})"
    )]
    ImportGenerationNamespaceChanged { expected: PathBuf, actual: PathBuf },
    #[error("direct-import projection-state path is not exactly private: {0}")]
    ImportGenerationPathNotPrivate(PathBuf),
    #[error("direct-import projection-state namespace is malformed or unsafe: {0}")]
    UnsafeImportGenerationNamespace(PathBuf),
    #[error("direct-import projection-state owner marker is invalid: {0}")]
    InvalidOwnerMarker(PathBuf),
    #[error("could not serialize direct-import projection-state owner marker: {0}")]
    SerializeOwner(serde_json::Error),
    #[error("could not write direct-import projection-state owner marker {path}: {source}")]
    WriteOwnerMarker {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not read direct-import projection-state owner marker {path}: {source}")]
    ReadOwnerMarker {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not safely access direct-import projection-state path {path}: {source}")]
    ScopedFilesystem {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("projection-state transfer attachment is missing")]
    TransferAttachmentMissing,
    #[error(
        "projection-state transfer attachment path changed (expected {expected}, got {actual})"
    )]
    TransferAttachmentPathChanged { expected: PathBuf, actual: PathBuf },
    #[error(
        "projection-state transfer path changed after validation (expected {expected}, got {actual})"
    )]
    TransferPathChanged { expected: PathBuf, actual: PathBuf },
    #[error("projection-state transfer schema does not match the sealed spool contract")]
    TransferSchemaMismatch,
    #[error("projection-state transfer accounting no longer matches its sealed descriptor")]
    TransferAccountingMismatch,
    #[error("projection-state transfer rows violate their source/car/digest contract")]
    TransferRowContractMismatch,
    #[error("projection-state transfer must contain exactly one selected-car row")]
    TransferCarContractMismatch,
    #[error("projection-state transfer attachment does not match the sealed spool descriptor")]
    TransferDigestMismatch,
    #[error("could not inspect free space for {path}: {source}")]
    FilesystemSpace {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("projection-state needs {required} free bytes but only {available} are available")]
    InsufficientFreeSpace { required: u64, available: u64 },
    #[error("projection-state SQLite error: {0}")]
    Sqlite(#[source] rusqlite::Error),
    #[error("projection-state row id must be positive")]
    InvalidRowId,
    #[error("projection-state car id must be positive")]
    InvalidCarId,
    #[error("projection-state received conflicting values for {entity:?} row {id}")]
    ConflictingRow {
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    },
    #[error("projection-state was modified concurrently at {entity:?} row {id}")]
    ConcurrentWrite {
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    },
    #[error("projection-state changed payload was modified concurrently at {entity:?} row {id}")]
    ConcurrentChangedWrite {
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    },
    #[error("projection-state row limit exceeded ({maximum})")]
    RowLimitExceeded { maximum: u64 },
    #[error("projection-state changed payload limit exceeded ({maximum} bytes)")]
    ChangedPayloadLimitExceeded { maximum: u64 },
    #[error(
        "projection-state changed payload row is too large ({payload_bytes} bytes; maximum {maximum})"
    )]
    ChangedPayloadRowLimitExceeded { maximum: u64, payload_bytes: u64 },
    #[error(
        "projection-state changed-page payload cap must be between 1 and {maximum} bytes (requested {requested})"
    )]
    InvalidChangedPagePayloadLimit { maximum: u64, requested: u64 },
    #[error("projection-state changed page exceeded its payload cap ({maximum} bytes)")]
    ChangedPagePayloadLimitExceeded { maximum: u64 },
    #[error("projection-state canonical payload must be UTF-8: {0}")]
    CanonicalPayloadUtf8(std::str::Utf8Error),
    #[error("could not serialize projection-state row: {0}")]
    SerializeRow(serde_json::Error),
    #[error("projection-state payload must serialize to a JSON object")]
    PayloadMustBeJsonObject,
    #[error("projection-state is sealed and cannot accept rows")]
    StateSealed,
    #[error("projection-state write batch failed and must be discarded")]
    WriteBatchFailed,
    #[error("projection-state must be sealed before it can be read")]
    StateNotSealed,
    #[error("projection-state page size must be between 1 and {MAX_PAGE_SIZE}")]
    InvalidPageSize,
    #[error("projection-state cursor id must be positive")]
    InvalidCursor,
    #[error("projection-state stored entity ordinal is invalid: {0}")]
    InvalidStoredEntity(i64),
    #[error("projection-state stored entity name is invalid: {0}")]
    InvalidStoredEntityName(String),
    #[error("projection-state stored digest is invalid")]
    InvalidStoredDigest,
    #[error("projection-state internal table selection is invalid")]
    InvalidStoredTable,
    #[error("projection-state accounting does not match its persisted rows")]
    PersistedAccountingMismatch,
    #[error("projection-state stored accounting is invalid")]
    InvalidStoredAccounting,
    #[error("projection-state stored changed payload byte accounting does not match its payload")]
    StoredChangedPayloadAccountingMismatch,
    #[error("projection-state stored changed payload does not match its canonical identity digest")]
    StoredChangedPayloadDigestMismatch,
    #[error("projection-state integrity check failed: {0}")]
    IntegrityCheckFailed(String),
    #[error("projection-state foreign-key check failed")]
    ForeignKeyCheckFailed,
    #[error("prior projection-state lookup failed: {0}")]
    PriorLookup(#[source] Box<dyn Error + Send + Sync>),
    #[error("could not remove private projection-state file {path}: {source}")]
    RemoveFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not remove private projection-state directory {path}: {source}")]
    RemoveDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
}
