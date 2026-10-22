// SPDX-License-Identifier: AGPL-3.0-only

// The schema steps of bulk B3 (work order section 4, "Every B3 schema step").
// C6 Part I owns the first one; C8 and C9 add theirs after it. The file is
// pulled into `migrations.rs` by `include!`, so it carries `//` and `///`
// comments only.

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

/// The undo of C6 Part I's step (C6 R63 (b) 3, C6 R69): it drops only what the
/// step adds. Every older rewind starts with it, because each rewind is
/// cumulative; the macro form lets the older macros place it first.
#[cfg(test)]
macro_rules! rewind_store_data_steps_sql {
    () => {
        "DROP TABLE IF EXISTS store_data_steps;"
    };
}

/// Test fixtures that rewind a current catalogue to the shape before C6 Part
/// I's step run this: it drops `store_data_steps` and the completed-step
/// markers in it. Every older rewind starts with the same drop.
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
