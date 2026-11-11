// SPDX-License-Identifier: AGPL-3.0-only

fn migrate(connection: &Connection) -> Result<(), StoreError> {
    admit_writable_catalogue(connection)?;
    let credential_fresh = schema_version(connection)? == 0;
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
    let version = schema_version(connection)?;
    if version == SCHEMA_VERSION {
        backup_pack_pins_table(connection)
    } else {
        Err(StoreError::UnsupportedSchema(version))
    }
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
