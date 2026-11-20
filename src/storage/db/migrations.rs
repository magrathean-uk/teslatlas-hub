// SPDX-License-Identifier: AGPL-3.0-only

fn migrate(connection: &Connection) -> Result<(), StoreError> {
    admit_writable_catalogue(connection)?;
    let credential_fresh = schema_version(connection)? == 0;
    // A floor or credential-format store (v78 or v79) passes the v80 guard
    // before the credential rung changes it, its v79 column repair included,
    // so a store v80 refuses is left exactly as it was (M03-T18). The guard
    // inside the v80 transaction stays.
    if (MIN_STORE_VERSION..=CREDENTIAL_FORMAT_SCHEMA_VERSION).contains(&schema_version(connection)?) {
        refuse_pre_seam_collected_rows(connection)?;
    }
    if schema_version(connection)? == 0 {
        let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
            .map_err(StoreError::Migrate)?;
        admit_writable_catalogue(&transaction)?;
        if schema_version(&transaction)? == 0 {
            transaction
                .execute_batch(
                    include_str!("schema_floor.sql")
                        .split_once("-- C10 credential schema")
                        .expect("C10 marker separates the retained floor from its suffix")
                        .0,
                )
                .map_err(StoreError::Migrate)?;
            transaction
                .pragma_update(None, "application_id", APPLICATION_ID)
                .map_err(StoreError::Migrate)?;
            transaction
                .pragma_update(None, "user_version", MIN_STORE_VERSION)
                .map_err(StoreError::Migrate)?;
        }
        transaction.commit().map_err(StoreError::Migrate)?;
    }
    credential_schema_rung(connection, credential_fresh)?;
    seam_schema_rung(connection)?;
    import_state_compaction_rung(connection)?;
    collector_retry_schema_rung(connection)?;
    // The v83 drive repair rung stays last: it upgrades whatever v80, v81 or
    // v82 store the earlier rungs leave.
    drive_repair_schema_rung(connection)?;
    let version = schema_version(connection)?;
    if version == SCHEMA_VERSION {
        backup_pack_pins_table(connection)?;
        drive_elevation_coverage_table(connection)
    } else {
        Err(StoreError::UnsupportedSchema(version))
    }
}

/// The version whose rung adds the import seam's tables and its guard.
const SEAM_SCHEMA_VERSION: i32 = 80;
const _: () = assert!(
    SEAM_SCHEMA_VERSION == CREDENTIAL_FORMAT_SCHEMA_VERSION + 1
        && SEAM_SCHEMA_VERSION <= SCHEMA_VERSION
);

/// The version whose rung keeps the collector's retry pacing in the account row.
const COLLECTOR_RETRY_SCHEMA_VERSION: i32 = 82;
const _: () = assert!(
    COLLECTOR_RETRY_SCHEMA_VERSION == IMPORT_STATE_COMPACT_SCHEMA_VERSION + 1
        && COLLECTOR_RETRY_SCHEMA_VERSION == SCHEMA_VERSION
);

/// Schema v82 (P04-F07, P04-F01): the account row keeps the end and the step of
/// the transport back-off and whether repeated list failures keep the inventory
/// from loading. The columns are added only where absent, in one transaction
/// with the version, so a rung that stops part way runs again on the next open.
fn collector_retry_schema_rung(connection: &Connection) -> Result<(), StoreError> {
    if schema_version(connection)? != IMPORT_STATE_COMPACT_SCHEMA_VERSION {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(StoreError::Migrate)?;
    admit_writable_catalogue(&transaction)?;
    if schema_version(&transaction)? == IMPORT_STATE_COMPACT_SCHEMA_VERSION {
        for (column, definition) in [
            (
                "unreachable_until_ms",
                "INTEGER CHECK(unreachable_until_ms IS NULL OR unreachable_until_ms >= 0)",
            ),
            (
                "consecutive_unreachable",
                "INTEGER NOT NULL DEFAULT 0 CHECK(consecutive_unreachable >= 0)",
            ),
            (
                "inventory_unavailable",
                "INTEGER NOT NULL DEFAULT 0 CHECK(inventory_unavailable IN (0, 1))",
            ),
        ] {
            let present: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('collector_account_state')
                                    WHERE name = ?1)",
                    [column],
                    |row| row.get(0),
                )
                .map_err(StoreError::Migrate)?;
            if !present {
                transaction
                    .execute_batch(&format!(
                        "ALTER TABLE collector_account_state ADD COLUMN {column} {definition}"
                    ))
                    .map_err(StoreError::Migrate)?;
            }
        }
        transaction
            .pragma_update(None, "user_version", COLLECTOR_RETRY_SCHEMA_VERSION)
            .map_err(StoreError::Migrate)?;
    }
    transaction.commit().map_err(StoreError::Migrate)
}

/// The materialised tables whose ids the import seam owns, with each one's
/// entity name in the import catalogues and its id column.
const SEAM_MATERIALISED_TABLES: [(&str, &str, &str); 6] = [
    ("materialised_drives", "drive", "drive_id"),
    ("materialised_charges", "charge", "charge_id"),
    ("materialised_charge_samples", "charge_sample", "sample_id"),
    ("materialised_positions", "position", "position_id"),
    ("materialised_states", "state", "state_id"),
    ("materialised_updates", "update", "update_id"),
];

/// Schema v80 (M03-T11): the import seam's decision table, the stored
/// collection coverage and the guard commit in one transaction. A store that
/// fails the guard is refused and keeps its version and every row; a rung that
/// stops part way rolls back and runs again on the next open. The tables are
/// created only where absent, as a synthetic image rewound below v80 keeps them.
fn seam_schema_rung(connection: &Connection) -> Result<(), StoreError> {
    if schema_version(connection)? != CREDENTIAL_FORMAT_SCHEMA_VERSION {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(StoreError::Migrate)?;
    admit_writable_catalogue(&transaction)?;
    if schema_version(&transaction)? == CREDENTIAL_FORMAT_SCHEMA_VERSION {
        refuse_pre_seam_collected_rows(&transaction)?;
        transaction
            .execute_batch(IMPORT_SEAM_LINKS_SCHEMA)
            .map_err(StoreError::Migrate)?;
        transaction
            .execute_batch(COLLECTION_COVERAGE_SCHEMA)
            .map_err(StoreError::Migrate)?;
        transaction
            .pragma_update(None, "user_version", SEAM_SCHEMA_VERSION)
            .map_err(StoreError::Migrate)?;
    }
    transaction.commit().map_err(StoreError::Migrate)
}

/// The v80 guard. A materialised row with an id at or above `HUB_ID_BASE`
/// (such as a row an earlier build moved by 2^50) or one absent from both
/// import catalogues (a row the Hub collected before the seam) cannot be told
/// apart from the ids the range now gives the Hub, so the store is refused,
/// naming the first such table and its count. The count is streamed SQL over
/// each table's key and the catalogues' covering keys.
fn refuse_pre_seam_collected_rows(connection: &Connection) -> Result<(), StoreError> {
    for (table, entity, column) in SEAM_MATERIALISED_TABLES {
        let sql = format!(
            "SELECT COUNT(*) FROM {table} AS m
             WHERE m.{column} >= ?1
                OR (NOT EXISTS(SELECT 1 FROM {legacy} AS l
                               WHERE l.vehicle_id = m.vehicle_id AND l.entity = ?2
                                 AND l.entity_id = m.{column})
                    AND NOT EXISTS(SELECT 1 FROM {current} AS c
                                   WHERE c.vehicle_id = m.vehicle_id AND c.entity_ordinal = ?3
                                     AND c.entity_id = m.{column}))",
            legacy = crate::db::import_table_names::import_projection_rows_table!(),
            current = crate::db::import_table_names::import_projection_state_rows_table!(),
        );
        // The durable state is read by its ordinal key, which both the v80
        // shape and the v81 shape (M08-T30) hold.
        let ordinal = i64::from(
            entity
                .parse::<SourceDbProjectionStateEntity>()
                .expect("each seam table names a projection state entity")
                .ordinal(),
        );
        let count: i64 = connection
            .query_row(
                &sql,
                params![hub_ids::HUB_ID_BASE, entity, ordinal],
                |row| row.get(0),
            )
            .map_err(StoreError::Migrate)?;
        if count != 0 {
            return Err(StoreError::PreSeamCollectedRows {
                table,
                count: u64::try_from(count).map_err(|_| StoreError::InvalidStoredCount)?,
            });
        }
    }
    Ok(())
}

/// The version whose rung rebuilds the import's per-row state without its
/// entity text and redundant UNIQUE index (M08-T30; design M08-T20,
/// recommendation 2, option d).
const IMPORT_STATE_COMPACT_SCHEMA_VERSION: i32 = 81;
const _: () = assert!(
    IMPORT_STATE_COMPACT_SCHEMA_VERSION == SEAM_SCHEMA_VERSION + 1
        && IMPORT_STATE_COMPACT_SCHEMA_VERSION <= SCHEMA_VERSION
);

/// The replacement table the v81 rung fills before its cut-over. It exists
/// only inside the rung's transaction.
macro_rules! import_state_rows_rebuild_table {
    () => {
        "import_projection_state_rows_rebuild"
    };
}

/// The `hub_metadata` key the cut-over writes and the compaction removes once
/// the catalogue is compacted. While it exists, every writable open owes the
/// compaction. Every store passing the rung, a new one included, is compacted
/// once, so a migrated catalogue lists its schema in the same order as a new
/// one (`VACUUM` rewrites `sqlite_schema` tables first, then indexes).
const IMPORT_STATE_COMPACTION_OWED_KEY: &str = "import_state_compaction_owed";

/// The v81 shape of the import's per-row state: every row digest with its
/// vehicle, entity ordinal, source id and selected car, keyed by the ordinal.
/// The entity text and the UNIQUE(vehicle_id, entity, entity_id) index of
/// the v78 floor are gone; the ordinal names the entity alone.
const IMPORT_STATE_ROWS_REBUILD_SCHEMA: &str = concat!(
    "CREATE TABLE ",
    import_state_rows_rebuild_table!(),
    " (
                    vehicle_id TEXT NOT NULL
                        REFERENCES \"",
    crate::db::import_table_names::import_projection_state_heads_table!(),
    "\"(vehicle_id) ON DELETE CASCADE,
                    entity_ordinal INTEGER NOT NULL CHECK(entity_ordinal BETWEEN 0 AND 6),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    projection_sha256 BLOB NOT NULL CHECK(length(projection_sha256) = 32),
                    PRIMARY KEY(vehicle_id, entity_ordinal, entity_id)
                ) STRICT, WITHOUT ROWID;"
);

/// Bytes kept free beyond the stated need, so the store's other writes and
/// the file system's own headroom survive the migration.
const IMPORT_STATE_COMPACTION_MARGIN_BYTES: u64 = 64 * 1024 * 1024;

/// The points of the v81 rung a test can interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImportStateCompactionStep {
    /// Inside the rebuild transaction, after the replacement is filled.
    Copied,
    /// Inside the rebuild transaction, after the old table is dropped.
    Dropped,
    /// Inside the rebuild transaction, after the rename and the version.
    CutOver,
    /// After the cut-over committed, before the compaction.
    Committed,
    /// After the compaction, before its marker is removed.
    Compacted,
}

#[cfg(test)]
thread_local! {
    static IMPORT_STATE_COMPACTION_FAULT: std::cell::Cell<Option<ImportStateCompactionStep>> =
        const { std::cell::Cell::new(None) };
    static IMPORT_STATE_COMPACTION_AVAILABLE_BYTES: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// Interrupt the next v81 rung on this thread at `step`, once.
#[cfg(test)]
pub(crate) fn interrupt_import_state_compaction_at(step: ImportStateCompactionStep) {
    IMPORT_STATE_COMPACTION_FAULT.with(|fault| fault.set(Some(step)));
}

/// Report `bytes` as the free space of every file system the v81 rung
/// budgets on this thread, or the real figures again with `None`.
#[cfg(test)]
pub(crate) fn set_import_state_compaction_available_bytes(bytes: Option<u64>) {
    IMPORT_STATE_COMPACTION_AVAILABLE_BYTES.with(|available| available.set(bytes));
}

#[cfg_attr(not(test), allow(clippy::unnecessary_wraps))]
fn import_state_compaction_checkpoint(
    #[cfg_attr(not(test), allow(unused_variables))] step: ImportStateCompactionStep,
) -> Result<(), StoreError> {
    #[cfg(test)]
    if IMPORT_STATE_COMPACTION_FAULT.with(|fault| fault.get()) == Some(step) {
        IMPORT_STATE_COMPACTION_FAULT.with(|fault| fault.set(None));
        return Err(StoreError::Migrate(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT),
            Some(format!("test interruption after {step:?}")),
        )));
    }
    Ok(())
}

/// Schema v81 (M08-T30): the import's per-row state loses its entity text and
/// its redundant UNIQUE index, and the catalogue is compacted. Two steps, each
/// resumable:
///
/// 1. One transaction fills a replacement table from the v80 table, proves
///    that every row, digest, source id and car binding is in it (equal
///    counts, no v80 row without its exact twin), drops the v80 table,
///    renames the replacement into place and sets the version. An
///    interruption anywhere rolls the whole step back: the store keeps its
///    v80 table and version, and the next writable open runs it again.
/// 2. The cut-over also leaves a marker in `hub_metadata`; while the marker
///    exists, every writable open compacts the catalogue
///    (`VACUUM`) and then removes the marker. An interruption before the
///    marker is removed compacts once more on the next open.
///
/// The old catalogue, the replacement and the journal stay on disk together
/// until the verified cut-over, and the compaction writes a further copy of
/// the catalogue (through the temporary directory and the journal), so the
/// rung refuses before writing when the free space cannot hold them.
fn import_state_compaction_rung(connection: &Connection) -> Result<(), StoreError> {
    if schema_version(connection)? == SEAM_SCHEMA_VERSION {
        rebuild_import_state_rows(connection)?;
    }
    if schema_version(connection)? >= IMPORT_STATE_COMPACT_SCHEMA_VERSION
        && import_state_compaction_owed(connection)?
    {
        compact_catalogue_after_import_state_rebuild(connection)?;
    }
    Ok(())
}

fn rebuild_import_state_rows(connection: &Connection) -> Result<(), StoreError> {
    let state_rows: i64 = connection
        .query_row(
            concat!(
                "SELECT EXISTS(SELECT 1 FROM ",
                crate::db::import_table_names::import_projection_state_rows_table!(),
                ")"
            ),
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::Migrate)?;
    if state_rows != 0 {
        // Replacement and journal each up to one catalogue beside the old
        // one, then the compaction's copy in the temporary directory.
        admit_import_state_compaction_budget(connection, 2, 1)?;
    }
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(StoreError::Migrate)?;
    admit_writable_catalogue(&transaction)?;
    if schema_version(&transaction)? == SEAM_SCHEMA_VERSION {
        transaction
            .execute_batch(concat!(
                "DROP TABLE IF EXISTS ",
                import_state_rows_rebuild_table!(),
                ";"
            ))
            .map_err(StoreError::Migrate)?;
        transaction
            .execute_batch(IMPORT_STATE_ROWS_REBUILD_SCHEMA)
            .map_err(StoreError::Migrate)?;
        let copied = transaction
            .execute(
                concat!(
                    "INSERT INTO ",
                    import_state_rows_rebuild_table!(),
                    "(vehicle_id, entity_ordinal, entity_id, car_id, projection_sha256)
                     SELECT vehicle_id, entity_ordinal, entity_id, car_id, projection_sha256
                       FROM ",
                    crate::db::import_table_names::import_projection_state_rows_table!(),
                    "
                      ORDER BY vehicle_id ASC, entity_ordinal ASC, entity_id ASC"
                ),
                [],
            )
            .map_err(StoreError::Migrate)?;
        import_state_compaction_checkpoint(ImportStateCompactionStep::Copied)?;
        // The v80 table's CHECK already binds each entity text to its
        // ordinal, so the ordinal loses nothing. The proof: as many rows on
        // each side, and no v80 row without its exact twin by primary key.
        let (old_rows, unmatched): (i64, i64) = transaction
            .query_row(
                concat!(
                    "SELECT COUNT(*),
                            COALESCE(SUM(NOT EXISTS(
                                SELECT 1 FROM ",
                    import_state_rows_rebuild_table!(),
                    " AS r
                                 WHERE r.vehicle_id = o.vehicle_id
                                   AND r.entity_ordinal = o.entity_ordinal
                                   AND r.entity_id = o.entity_id
                                   AND r.car_id = o.car_id
                                   AND r.projection_sha256 = o.projection_sha256)), 0)
                       FROM ",
                    crate::db::import_table_names::import_projection_state_rows_table!(),
                    " AS o"
                ),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(StoreError::Migrate)?;
        let rebuilt_rows: i64 = transaction
            .query_row(
                concat!("SELECT COUNT(*) FROM ", import_state_rows_rebuild_table!()),
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Migrate)?;
        if unmatched != 0
            || old_rows != rebuilt_rows
            || usize::try_from(old_rows).ok() != Some(copied)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        transaction
            .execute_batch(concat!(
                "DROP TABLE \"",
                crate::db::import_table_names::import_projection_state_rows_table!(),
                "\";"
            ))
            .map_err(StoreError::Migrate)?;
        import_state_compaction_checkpoint(ImportStateCompactionStep::Dropped)?;
        transaction
            .execute_batch(concat!(
                "ALTER TABLE ",
                import_state_rows_rebuild_table!(),
                " RENAME TO \"",
                crate::db::import_table_names::import_projection_state_rows_table!(),
                "\";"
            ))
            .map_err(StoreError::Migrate)?;
        transaction
            .execute(
                "INSERT INTO hub_metadata(key, value) VALUES(?1, 'v81')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![IMPORT_STATE_COMPACTION_OWED_KEY],
            )
            .map_err(StoreError::Migrate)?;
        transaction
            .pragma_update(None, "user_version", IMPORT_STATE_COMPACT_SCHEMA_VERSION)
            .map_err(StoreError::Migrate)?;
        import_state_compaction_checkpoint(ImportStateCompactionStep::CutOver)?;
    }
    transaction.commit().map_err(StoreError::Migrate)?;
    import_state_compaction_checkpoint(ImportStateCompactionStep::Committed)
}

fn import_state_compaction_owed(connection: &Connection) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM hub_metadata WHERE key = ?1)",
            params![IMPORT_STATE_COMPACTION_OWED_KEY],
            |row| row.get(0),
        )
        .map_err(StoreError::Migrate)
}

/// Compacts the catalogue once the cut-over has committed: `VACUUM` rewrites
/// it without the v80 table's free pages, a checkpoint moves the result into
/// the database file and empties the journal, and the marker goes last.
fn compact_catalogue_after_import_state_rebuild(connection: &Connection) -> Result<(), StoreError> {
    // The copy in the temporary directory and the journal beside the
    // catalogue, each up to one catalogue.
    admit_import_state_compaction_budget(connection, 1, 1)?;
    connection
        .execute_batch("VACUUM;")
        .map_err(StoreError::Migrate)?;
    import_state_compaction_checkpoint(ImportStateCompactionStep::Compacted)?;
    connection
        .execute(
            "DELETE FROM hub_metadata WHERE key = ?1",
            params![IMPORT_STATE_COMPACTION_OWED_KEY],
        )
        .map_err(StoreError::Migrate)?;
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(StoreError::Migrate)
}

/// Refuses, before anything is written, when the catalogue's file system
/// cannot hold `beside` further catalogues plus the margin, or the temporary
/// directory `temporary` of them. The catalogue's size is its page count
/// times its page size; the journal already on disk counts against the free
/// space the file system reports.
fn admit_import_state_compaction_budget(
    connection: &Connection,
    beside: u64,
    temporary: u64,
) -> Result<(), StoreError> {
    let Some(path) = connection.path().filter(|path| !path.is_empty()) else {
        // An in-memory catalogue has no file system to budget.
        return Ok(());
    };
    let catalogue_bytes: u64 = connection
        .query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(StoreError::Migrate)
        .and_then(|bytes| u64::try_from(bytes).map_err(|_| StoreError::InvalidStoredCount))?;
    let directory = Path::new(path)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    for (location, catalogues) in [(directory, beside), (std::env::temp_dir(), temporary)] {
        let needed = catalogue_bytes
            .saturating_mul(catalogues)
            .saturating_add(IMPORT_STATE_COMPACTION_MARGIN_BYTES);
        let available = import_state_compaction_available_bytes(&location)?;
        if available < needed {
            return Err(StoreError::Migrate(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
                Some(format!(
                    "the import state compaction needs {needed} bytes free in {}, \
                     {available} available; nothing was changed",
                    location.display()
                )),
            )));
        }
    }
    Ok(())
}

fn import_state_compaction_available_bytes(location: &Path) -> Result<u64, StoreError> {
    #[cfg(test)]
    if let Some(bytes) = IMPORT_STATE_COMPACTION_AVAILABLE_BYTES.with(std::cell::Cell::get) {
        return Ok(bytes);
    }
    let stat = rustix::fs::statvfs(location).map_err(|errno| {
        StoreError::Migrate(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some(format!(
                "cannot read the free space of {}: {errno}",
                location.display()
            )),
        ))
    })?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// M06-T07: the pin rows of running encrypted backups
/// (`data_recovery::encrypted_backup`) are part of the schema, and so are the
/// recorded runs of M06-T02's retention (`backup_runs`). Every current store
/// gains the tables here, in place and once; a store whose first encrypted
/// backup created them on first use keeps them unchanged. The tables hold
/// live-host state only: every sealed copy and restored catalogue keeps the
/// tables and drops their rows.
const BACKUP_PACK_PINS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS backup_pack_pins (
    pin_id TEXT NOT NULL CHECK(length(pin_id) = 36),
    sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > 0),
    PRIMARY KEY(pin_id, sha256)
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS backup_runs (
    generation TEXT PRIMARY KEY CHECK(length(generation) = 36),
    destination BLOB NOT NULL,
    file_device TEXT NOT NULL,
    file_inode TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;";

fn backup_pack_pins_table(connection: &Connection) -> Result<(), StoreError> {
    let present = |connection: &Connection| -> Result<bool, StoreError> {
        connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN ('backup_pack_pins', 'backup_runs')) = 2",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Migrate)
    };
    if present(connection)? {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(StoreError::Migrate)?;
    admit_writable_catalogue(&transaction)?;
    if schema_version(&transaction)? == SCHEMA_VERSION && !present(&transaction)? {
        transaction
            .execute_batch(BACKUP_PACK_PINS_SCHEMA)
            .map_err(StoreError::Migrate)?;
    }
    transaction.commit().map_err(StoreError::Migrate)
}

/// Side metadata of issue #32: the elevation coverage of a Hub-derived drive's
/// ascent and descent, beside `materialised_drives` and never inside its
/// `drive_json`, so the sync payloads and pack rows keep their shape. No row is
/// unknown; deleting the drive deletes its coverage.
const DRIVE_ELEVATION_COVERAGE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS drive_elevation_coverage (
    vehicle_id TEXT NOT NULL,
    drive_id INTEGER NOT NULL,
    coverage TEXT NOT NULL CHECK (coverage IN ('complete', 'partial')),
    PRIMARY KEY (vehicle_id, drive_id)
) STRICT, WITHOUT ROWID;
CREATE TRIGGER IF NOT EXISTS drive_elevation_coverage_follows_drive
AFTER DELETE ON materialised_drives
BEGIN
    DELETE FROM drive_elevation_coverage
    WHERE vehicle_id = OLD.vehicle_id AND drive_id = OLD.drive_id;
END;";

fn drive_elevation_coverage_table(connection: &Connection) -> Result<(), StoreError> {
    let present = |connection: &Connection| -> Result<bool, StoreError> {
        connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM sqlite_schema
                 WHERE (type = 'table' AND name = 'drive_elevation_coverage')
                    OR (type = 'trigger' AND name = 'drive_elevation_coverage_follows_drive')) = 2",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Migrate)
    };
    if present(connection)? {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(StoreError::Migrate)?;
    admit_writable_catalogue(&transaction)?;
    if schema_version(&transaction)? == SCHEMA_VERSION && !present(&transaction)? {
        transaction
            .execute_batch(DRIVE_ELEVATION_COVERAGE_SCHEMA)
            .map_err(StoreError::Migrate)?;
    }
    transaction.commit().map_err(StoreError::Migrate)
}
