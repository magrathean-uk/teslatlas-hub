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
    let version = schema_version(connection)?;
    if version == SCHEMA_VERSION {
        catalogue_v80_tables(connection)
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
                                   WHERE c.vehicle_id = m.vehicle_id AND c.entity = ?2
                                     AND c.entity_id = m.{column}))",
            legacy = crate::db::import_table_names::import_projection_rows_table!(),
            current = crate::db::import_table_names::import_projection_state_rows_table!(),
        );
        let count: i64 = connection
            .query_row(&sql, params![hub_ids::HUB_ID_BASE, entity], |row| row.get(0))
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

/// M06-T07: the pin rows of running encrypted backups
/// (`data_recovery::encrypted_backup`) are part of the schema, and so are the
/// recorded runs of M06-T02's retention (`backup_runs`). Every current store
/// gains the tables here, in place and once; a store whose first encrypted
/// backup created them on first use keeps them unchanged. The backup tables hold
/// live-host state only: every sealed copy and restored catalogue keeps the
/// tables and drops their rows.
/// Polling replay records travel with the current observations they protect.
const CATALOGUE_V80_TABLES: &str = "CREATE TABLE IF NOT EXISTS backup_pack_pins (
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
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS poll_observation_replays (
    vehicle_id TEXT NOT NULL,
    record_type TEXT NOT NULL,
    source_id TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256) = 32),
    observation_id INTEGER NOT NULL,
    received_at_ms INTEGER NOT NULL,
    PRIMARY KEY(vehicle_id, record_type, source_id, observed_at_ms, payload_sha256),
    FOREIGN KEY(vehicle_id, record_type)
        REFERENCES current_observations(vehicle_id, record_type) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;";

fn catalogue_v80_tables(connection: &Connection) -> Result<(), StoreError> {
    let present = |connection: &Connection| -> Result<bool, StoreError> {
        connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN (
                     'backup_pack_pins', 'backup_runs', 'poll_observation_replays')) = 3",
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
            .execute_batch(CATALOGUE_V80_TABLES)
            .map_err(StoreError::Migrate)?;
    }
    transaction.commit().map_err(StoreError::Migrate)
}
