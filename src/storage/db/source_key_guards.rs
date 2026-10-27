// SPDX-License-Identifier: AGPL-3.0-only

fn stored_import_source_keys(connection: &Connection) -> Result<Vec<String>, StoreError> {
    connection
        .prepare(
            "SELECT source_key FROM source_identities WHERE source_kind=?1 ORDER BY source_key",
        )
        .map_err(StoreError::Query)?
        .query_map([IMPORT_SOURCE_KIND], |row| row.get(0))
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)
}

fn require_import_source_key(
    connection: &Connection,
    key: &str,
    new_import_source: bool,
) -> Result<(), StoreError> {
    require_import_source_key_from_stored(
        &stored_import_source_keys(connection)?,
        key,
        new_import_source,
    )
}

fn require_import_source_key_from_stored(
    keys: &[String],
    key: &str,
    new_import_source: bool,
) -> Result<(), StoreError> {
    if !new_import_source && !keys.is_empty() && !keys.iter().any(|stored| stored == key) {
        return Err(StoreError::UnmatchedImportSource {
            stored_keys: keys.join(", "),
        });
    }
    Ok(())
}

impl HubStore {
    /// Refuse a changed source key before startup recovery or profile preparation.
    /// The existing catalogue is neither migrated nor repaired. SQLite may
    /// coordinate a WAL reader, as for the ordinary read-only opening contract.
    pub fn preflight_import_source_key(
        data_dir: impl AsRef<Path>,
        key: &str,
        new_import_source: bool,
    ) -> Result<(), StoreError> {
        let data_dir = data_dir.as_ref();
        let database_path = data_dir.join("hub.sqlite");
        let metadata = match fs::symlink_metadata(&database_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(StoreError::InspectSharedSqlite(error)),
        };
        validate_private_store_directory(data_dir)
            .map_err(|()| StoreError::UnsafeDataDir(data_dir.to_path_buf()))?;
        admit_shared_sqlite_file(
            &database_path,
            &metadata,
            shared_sqlite_group_id(&database_path)?,
            false,
        )?;
        admit_shared_sqlite_sidecars(&database_path, false)?;
        let connection = Connection::open_with_flags(
            &database_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::Open)?;
        configure_read_only(&connection)?;
        // This existing identity/version admission performs queries only, even
        // though ordinary startup uses it before opening a writable catalogue.
        admit_writable_catalogue(&connection)?;
        let version = schema_version(&connection)?;
        let empty: bool = connection
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema)",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        if version == 0 && empty {
            return Ok(());
        }
        let keys = stored_import_source_keys(&connection)?;
        require_import_source_key_from_stored(&keys, key, new_import_source)
    }

    /// Read-only admission before import preparation. Registration repeats the
    /// same guard in its IMMEDIATE transaction to close concurrent races.
    pub fn check_import_source_key(
        &self,
        key: &str,
        new_import_source: bool,
    ) -> Result<(), StoreError> {
        require_import_source_key(&self.open_read_only_connection()?, key, new_import_source)
    }

    pub fn register_import_source(
        &self,
        key: &str,
        created_at_ms: i64,
        new_import_source: bool,
    ) -> Result<SourceRecord, StoreError> {
        self.register_source_db_import_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, key),
            created_at_ms,
            new_import_source,
        )
        .map(|(source, _)| source)
    }

    /// A current reader must bind an existing source, including on an empty
    /// store. Its error lists stored keys and never echoes the requested key.
    pub fn check_current_import_source_key(&self, key: &str) -> Result<(), StoreError> {
        let keys = stored_import_source_keys(&self.open_read_only_connection()?)?;
        if keys.iter().any(|stored| stored == key) {
            Ok(())
        } else {
            Err(StoreError::UnmatchedCurrentImportSource {
                stored_keys: keys.join(", "),
            })
        }
    }
}
