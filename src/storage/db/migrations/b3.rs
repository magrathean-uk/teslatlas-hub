// SPDX-License-Identifier: AGPL-3.0-only

// The schema steps of bulk B3 (work order section 4, "Every B3 schema step").
// C6 Part I owns the first one; C8 and C9 add theirs after it. The file is
// pulled into `migrations.rs` by `include!`, so it carries `//` and `///`
// comments only. A `macro_rules!` is textually scoped, so the geofence undo
// is defined first: Part I's rewind macro expands it.

/// The shape before the geofence step (C8 R49): the two state tables go and
/// `geofences` is rebuilt without `source_owned`, with the checks the step
/// replaces. The checks are the ones of the steps that created them: the name
/// limit of 256 bytes and the radius bound of 5000 m (the v12 step) and the
/// non-negative session fee (the v19 step). Statements only, so it runs inside
/// or outside a caller's transaction, on a store whose rows fit those checks.
#[cfg(test)]
macro_rules! rewind_v76_sql {
    () => {
        "DROP TABLE IF EXISTS deleted_imported_geofences;
             DROP TABLE IF EXISTS geofence_id_marks;
             CREATE TABLE geofences_predecessor (
                 vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                 source_geofence_id INTEGER NOT NULL CHECK(source_geofence_id > 0),
                 name TEXT NOT NULL CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 256),
                 latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                 longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                 radius_m REAL NOT NULL CHECK(radius_m > 0.0 AND radius_m <= 5000.0),
                 billing_type TEXT
                     CHECK(billing_type IS NULL OR billing_type IN ('per_kwh', 'per_minute')),
                 cost_per_unit REAL,
                 session_fee REAL CHECK(session_fee IS NULL OR session_fee >= 0.0),
                 PRIMARY KEY(vehicle_id, source_geofence_id)
             ) STRICT;
             INSERT INTO geofences_predecessor(
                 vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                 billing_type, cost_per_unit, session_fee)
             SELECT vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                    billing_type, cost_per_unit, session_fee
               FROM geofences;
             DROP TABLE geofences;
             ALTER TABLE geofences_predecessor RENAME TO geofences;
             CREATE INDEX geofences_vehicle_location
                 ON geofences(vehicle_id, latitude, longitude);
            "
    };
}

/// The version C6 Part I's step writes (C6 R63 (b) 4): the table of completed
/// one-time data steps. The step's version check, its `PRAGMA user_version`
/// and its own upgrade tests use this name and never a bare number; a test's
/// predecessor version is this constant minus one.
pub(crate) const STORE_DATA_STEPS_SCHEMA_VERSION: i32 = 75;

/// C6 R57's table, issued as `CREATE TABLE IF NOT EXISTS` (C6 R69). A row is
/// inserted only in the transaction that completes the step it names and is
/// never updated or deleted by production code, so a present row means done
/// (C6 R63 (a)). A step that runs in chunks or can be re-armed keeps its own
/// state elsewhere.
const STORE_DATA_STEPS_TABLE_SQL: &str = "
    CREATE TABLE IF NOT EXISTS store_data_steps (
        step_key TEXT PRIMARY KEY NOT NULL,
        completed_at_ms INTEGER NOT NULL,
        summary_json TEXT NOT NULL CHECK(json_valid(summary_json))
    ) STRICT;
";

/// The undo of C6 Part I's step (C6 R63 (b) 3, C6 R69) as every older rewind
/// starts it: the undo of the geofence step that follows it (C8 R49,
/// `rewind_v76_sql!`), then the drop of what Part I's step adds. Each rewind is
/// cumulative, so a later step's undo goes first (C8 R49 item 3, C6 R63 (b) 3); the macro form lets
/// the older macros place the pair first.
#[cfg(test)]
macro_rules! rewind_store_data_steps_sql {
    () => {
        concat!(rewind_v76_sql!(), "DROP TABLE IF EXISTS store_data_steps;")
    };
}

/// Test fixtures that rewind a current catalogue to the shape before C6 Part
/// I's step run this: the geofence undo, then the drop of `store_data_steps`
/// and the completed-step markers in it. Every older rewind starts with the
/// same pair. The geofence step's own rewind alone is [`REWIND_V76_SQL`].
#[cfg(test)]
pub(crate) const REWIND_V75_SQL: &str = rewind_store_data_steps_sql!();

/// C6 Part I's step (C6 R57, R63): add the table of completed one-time data
/// steps. Additive and not backfilled; the identity tidy that follows the last
/// migration inserts the first row.
fn migrate_store_data_steps_table(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {STORE_DATA_STEPS_TABLE_SQL} \
             PRAGMA user_version = {STORE_DATA_STEPS_SCHEMA_VERSION}; COMMIT;"
        ))
        .map_err(StoreError::Migrate)
}

// C8 Part S-a: the geofence schema step (C8 R12, C8 R49).

/// The schema version the geofence step writes (C8 R12). It is the step's
/// version check, its `PRAGMA user_version` and, minus one, the version its
/// own tests start from.
pub(crate) const GEOFENCE_STEP_SCHEMA_VERSION: i32 = 76;

/// The `geofences` table the geofence step builds (C8 section 1.3): the name
/// limit widens to the stored byte bound, the radius bound to the stored
/// bound, the session fee check goes, and `source_owned` is added. The name
/// check counts bytes because SQLite's `length()` of text counts characters.
fn rebuilt_geofences_table_sql() -> String {
    format!(
        "CREATE TABLE geofences_rebuilt (
            vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
            source_geofence_id INTEGER NOT NULL CHECK(source_geofence_id > 0),
            name TEXT NOT NULL
                CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND {name_bytes}),
            latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
            longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
            radius_m REAL NOT NULL CHECK(radius_m > 0.0 AND radius_m <= {radius_bound:?}),
            billing_type TEXT
                CHECK(billing_type IS NULL OR billing_type IN ('per_kwh', 'per_minute')),
            cost_per_unit REAL,
            session_fee REAL,
            source_owned INTEGER NOT NULL DEFAULT 0 CHECK(source_owned IN (0, 1)),
            PRIMARY KEY(vehicle_id, source_geofence_id)
        ) STRICT;",
        name_bytes = crate::geo::location::MAX_GEOFENCE_NAME_BYTES,
        // The stored bound is the larger of the previous bound (5000 m) and the
        // largest radius an import activates.
        radius_bound = crate::geo::location::MAX_IMPORTED_GEOFENCE_RADIUS_METRES,
    )
}

/// The two tables the geofence step adds, empty (C8 section 1.3): the highest
/// id the Hub has allocated per vehicle, and the imported ids the owner
/// deleted in the Hub. The id bounds are the single definitions of C8 section
/// 1.1.
fn geofence_state_tables_sql() -> String {
    format!(
        "CREATE TABLE geofence_id_marks (
            vehicle_id TEXT PRIMARY KEY NOT NULL
                REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
            last_hub_id INTEGER NOT NULL CHECK(last_hub_id >= {first_hub_id})
        ) STRICT;
        CREATE TABLE deleted_imported_geofences (
            vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
            source_geofence_id INTEGER NOT NULL
                CHECK(source_geofence_id BETWEEN 1 AND {max_imported_id}),
            PRIMARY KEY(vehicle_id, source_geofence_id)
        ) STRICT;",
        first_hub_id = crate::geo::location::FIRST_HUB_GEOFENCE_ID,
        max_imported_id = crate::geo::location::MAX_IMPORTED_GEOFENCE_ID,
    )
}

/// v75 -> v76 (C8 R12): rebuild `geofences` by SQLite's documented table
/// rebuild (create the new table, copy every row with an explicit column list,
/// drop the old table, rename the new one, recreate the index), create the two
/// state tables, and check the foreign keys of the rebuilt table. One
/// transaction; any failure, including a row that cannot be copied, rolls
/// everything back. No table references `geofences`, so foreign keys stay on.
/// No history is relabelled (C8 R33).
fn migrate_geofence_step(connection: &Connection) -> Result<(), StoreError> {
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(StoreError::Migrate)?;
    transaction
        .execute_batch(&rebuilt_geofences_table_sql())
        .map_err(StoreError::Migrate)?;
    transaction
        .execute_batch(
            "INSERT INTO geofences_rebuilt(
                vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                billing_type, cost_per_unit, session_fee, source_owned)
             SELECT vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                    billing_type, cost_per_unit, session_fee, 0
               FROM geofences;
             DROP TABLE geofences;
             ALTER TABLE geofences_rebuilt RENAME TO geofences;
             CREATE INDEX geofences_vehicle_location
                 ON geofences(vehicle_id, latitude, longitude);",
        )
        .map_err(StoreError::Migrate)?;
    transaction
        .execute_batch(&geofence_state_tables_sql())
        .map_err(StoreError::Migrate)?;
    let violations: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM pragma_foreign_key_check('geofences')",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::Migrate)?;
    if violations != 0 {
        return Err(StoreError::Migrate(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
            Some("rebuilt geofences table has foreign key violations".to_owned()),
        )));
    }
    transaction
        .execute_batch(&format!(
            "PRAGMA user_version = {GEOFENCE_STEP_SCHEMA_VERSION};"
        ))
        .map_err(StoreError::Migrate)?;
    transaction.commit().map_err(StoreError::Migrate)
}

/// Test fixtures that rewind a current catalogue to the shape before the
/// geofence step run this.
#[cfg(test)]
pub(crate) const REWIND_V76_SQL: &str = rewind_v76_sql!();
