// SPDX-License-Identifier: AGPL-3.0-only

// Durable receiver state (G2-6 6A): the accumulator-state table of the v73
// step, its two accessors, and the v74 step that copies the retired delivery
// consumer's gap evidence and drops its objects. The historical object
// definitions live here as neutral constants so the migration chain and the
// rewind fixtures share one text.

/// Historical tables and indexes created by the v57 step, kept verbatim so
/// older catalogues still migrate through the chain before the v74 step drops
/// them.
macro_rules! historical_delivery_tables_sql {
    () => {
        "
    CREATE TABLE IF NOT EXISTS edge_lineages (
        installation_id TEXT NOT NULL,
        lineage TEXT NOT NULL,
        source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
        vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
        vin TEXT NOT NULL,
        car_id INTEGER NOT NULL CHECK(car_id > 0),
        first_spool_seq INTEGER,
        ack_frontier INTEGER,
        created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
        updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= created_at_ms),
        PRIMARY KEY(installation_id, lineage),
        CHECK(length(CAST(installation_id AS BLOB)) BETWEEN 1 AND 128),
        CHECK(length(CAST(lineage AS BLOB)) BETWEEN 1 AND 128),
        CHECK(length(CAST(vin AS BLOB)) = 17),
        CHECK(first_spool_seq IS NULL OR first_spool_seq >= 1),
        CHECK(ack_frontier IS NULL OR ack_frontier >= first_spool_seq)
    ) STRICT;
    CREATE TABLE IF NOT EXISTS edge_applications (
        installation_id TEXT NOT NULL,
        lineage TEXT NOT NULL,
        stable_record_id TEXT NOT NULL,
        payload_sha256 TEXT NOT NULL,
        disposition TEXT NOT NULL CHECK(disposition IN (
            'projected_telemetry', 'durable_non_projection_event'
        )),
        first_spool_seq INTEGER NOT NULL CHECK(first_spool_seq >= 1),
        observation_id INTEGER,
        applied_at_ms INTEGER NOT NULL CHECK(applied_at_ms >= 0),
        PRIMARY KEY(installation_id, lineage, stable_record_id),
        FOREIGN KEY(installation_id, lineage)
            REFERENCES edge_lineages(installation_id, lineage) ON DELETE RESTRICT,
        CHECK(length(stable_record_id) = 64),
        CHECK(length(payload_sha256) = 64)
    ) STRICT;
    CREATE TABLE IF NOT EXISTS edge_sequence_dispositions (
        installation_id TEXT NOT NULL,
        lineage TEXT NOT NULL,
        spool_seq INTEGER NOT NULL CHECK(spool_seq >= 1),
        item_kind TEXT NOT NULL CHECK(item_kind IN ('record', 'gap')),
        item_id TEXT NOT NULL,
        legacy_record_id TEXT,
        payload_sha256 TEXT,
        category TEXT NOT NULL CHECK(category IN (
            'projected_telemetry', 'durable_non_projection_event',
            'durable_gap', 'duplicate'
        )),
        reason TEXT NOT NULL,
        occurred_at_ms INTEGER,
        evidence_sha256 TEXT,
        committed_at_ms INTEGER NOT NULL CHECK(committed_at_ms >= 0),
        PRIMARY KEY(installation_id, lineage, spool_seq),
        FOREIGN KEY(installation_id, lineage)
            REFERENCES edge_lineages(installation_id, lineage) ON DELETE RESTRICT,
        CHECK(length(item_id) = 64),
        CHECK(legacy_record_id IS NULL OR length(legacy_record_id) = 64),
        CHECK(payload_sha256 IS NULL OR length(payload_sha256) = 64),
        CHECK(evidence_sha256 IS NULL OR length(evidence_sha256) = 64),
        CHECK((item_kind = 'gap' AND category = 'durable_gap'
               AND legacy_record_id IS NULL AND payload_sha256 IS NULL
               AND occurred_at_ms IS NOT NULL AND evidence_sha256 IS NOT NULL)
           OR (item_kind = 'record' AND category != 'durable_gap'
               AND legacy_record_id IS NOT NULL AND payload_sha256 IS NOT NULL
               AND occurred_at_ms IS NULL AND evidence_sha256 IS NULL))
    ) STRICT;
    CREATE UNIQUE INDEX IF NOT EXISTS edge_sequence_item_identity
        ON edge_sequence_dispositions(installation_id, lineage, item_kind, item_id, spool_seq);
    CREATE TABLE IF NOT EXISTS edge_accumulator_states (
        installation_id TEXT NOT NULL,
        lineage TEXT NOT NULL,
        vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
        state_version INTEGER NOT NULL CHECK(state_version = 1),
        state_json BLOB NOT NULL CHECK(length(state_json) BETWEEN 2 AND 262144),
        through_spool_seq INTEGER NOT NULL CHECK(through_spool_seq >= 1),
        updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0),
        PRIMARY KEY(installation_id, lineage),
        FOREIGN KEY(installation_id, lineage)
            REFERENCES edge_lineages(installation_id, lineage) ON DELETE RESTRICT
    ) STRICT;
    CREATE TABLE IF NOT EXISTS edge_pending_publications (
        installation_id TEXT NOT NULL,
        lineage TEXT NOT NULL,
        stable_record_id TEXT NOT NULL,
        vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
        status TEXT NOT NULL CHECK(status IN ('pending', 'publishing', 'retry', 'complete')),
        attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
        next_attempt_ms INTEGER NOT NULL CHECK(next_attempt_ms >= 0),
        last_error TEXT,
        created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
        completed_at_ms INTEGER,
        PRIMARY KEY(installation_id, lineage, stable_record_id),
        FOREIGN KEY(installation_id, lineage, stable_record_id)
            REFERENCES edge_applications(installation_id, lineage, stable_record_id)
            ON DELETE RESTRICT,
        CHECK(last_error IS NULL OR length(CAST(last_error AS BLOB)) <= 256)
    ) STRICT;
    CREATE INDEX IF NOT EXISTS edge_pending_publication_due
        ON edge_pending_publications(status, next_attempt_ms);
    CREATE TABLE IF NOT EXISTS edge_consumer_diagnostics (
        installation_id TEXT NOT NULL,
        lineage TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN (
            'connected', 'backoff', 'auth_failed', 'blocked_record',
            'pending_publication', 'stopped'
        )),
        detail TEXT NOT NULL,
        updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0),
        PRIMARY KEY(installation_id, lineage),
        CHECK(length(CAST(detail AS BLOB)) <= 256)
    ) STRICT;
"
    };
}

#[cfg(test)]
macro_rules! historical_delivery_triggers_sql {
    () => {
        "
CREATE TRIGGER IF NOT EXISTS edge_lineages_require_car_id
BEFORE INSERT ON edge_lineages
FOR EACH ROW WHEN NEW.car_id IS NULL OR NEW.car_id <= 0
BEGIN
    SELECT RAISE(ABORT, 'edge lineage car identity is required');
END;
CREATE TRIGGER IF NOT EXISTS edge_lineages_immutable_car_id
BEFORE UPDATE OF car_id ON edge_lineages
FOR EACH ROW WHEN OLD.car_id IS NOT NEW.car_id
BEGIN
    SELECT RAISE(ABORT, 'edge lineage car identity is immutable');
END;
"
    };
}

const HISTORICAL_DELIVERY_TABLES_SQL: &str = historical_delivery_tables_sql!();

/// Every object the v74 step drops, in drop order.
const RETIRED_DELIVERY_DROPS_SQL: &str = "
    DROP TRIGGER IF EXISTS edge_lineages_require_car_id;
    DROP TRIGGER IF EXISTS edge_lineages_immutable_car_id;
    DROP INDEX IF EXISTS edge_pending_publication_due;
    DROP INDEX IF EXISTS edge_sequence_item_identity;
    DROP TABLE IF EXISTS edge_pending_publications;
    DROP TABLE IF EXISTS edge_consumer_diagnostics;
    DROP TABLE IF EXISTS edge_accumulator_states;
    DROP TABLE IF EXISTS edge_sequence_dispositions;
    DROP TABLE IF EXISTS edge_applications;
    DROP TABLE IF EXISTS edge_lineages;
";

const RETIRED_DELIVERY_GAPS_SQL: &str = "
    CREATE TABLE retired_delivery_gaps (
        retired_gap_id INTEGER PRIMARY KEY,
        installation_id TEXT NOT NULL,
        lineage TEXT NOT NULL,
        spool_seq INTEGER NOT NULL CHECK(spool_seq >= 1),
        item_id TEXT NOT NULL CHECK(length(item_id) = 64),
        reason TEXT NOT NULL,
        occurred_at_ms INTEGER,
        evidence_sha256 TEXT
            CHECK(evidence_sha256 IS NULL OR length(evidence_sha256) = 64),
        committed_at_ms INTEGER NOT NULL CHECK(committed_at_ms >= 0),
        vehicle_id TEXT REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
        copied_at_ms INTEGER NOT NULL CHECK(copied_at_ms >= 0),
        UNIQUE(installation_id, lineage, spool_seq)
    ) STRICT;
";

const RECEIVER_STATE_TABLE_SQL: &str = "
    CREATE TABLE fleet_telemetry_accumulator_states (
        vehicle_id TEXT PRIMARY KEY NOT NULL
            REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
        state_version INTEGER NOT NULL CHECK(state_version = 2),
        state_json BLOB NOT NULL
            CHECK(length(state_json) BETWEEN 2 AND 262144),
        updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
    ) STRICT;
";

/// Test fixtures that rewind a current catalogue to the shape before the v74
/// step: the gap table goes and the retired objects return, empty.
#[cfg(test)]
macro_rules! rewind_v74_sql {
    () => {
        concat!(
            "DROP TABLE IF EXISTS retired_delivery_gaps;",
            historical_delivery_tables_sql!(),
            historical_delivery_triggers_sql!()
        )
    };
}

/// The shape before the v73 step: everything the v74 step adds, then the
/// accumulator-state table.
#[cfg(test)]
macro_rules! rewind_v73_sql {
    () => {
        concat!(
            rewind_v74_sql!(),
            "DROP TABLE IF EXISTS fleet_telemetry_accumulator_states;"
        )
    };
}

/// Where a test makes the v74 step fail, to prove the whole step rolls back.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetirementFault {
    GapCopy,
    OutboxMarking,
    TableDrops,
}

#[cfg(test)]
thread_local! {
    static RETIREMENT_FAULT: std::cell::Cell<Option<RetirementFault>> =
        const { std::cell::Cell::new(None) };
}

/// Arm (or clear) the v74 step's failure point for migrations that run on this
/// thread. Migration runs on the opening thread, so a thread-local seam is
/// enough.
#[cfg(test)]
pub(crate) fn set_retirement_fault(fault: Option<RetirementFault>) {
    RETIREMENT_FAULT.with(|slot| slot.set(fault));
}

#[cfg(test)]
fn retirement_fault_hit(point: RetirementFault) -> Result<(), rusqlite::Error> {
    if RETIREMENT_FAULT.with(std::cell::Cell::get) == Some(point) {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some("injected retirement fault".to_owned()),
        ));
    }
    Ok(())
}

fn migrate_receiver_state_table(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {RECEIVER_STATE_TABLE_SQL} PRAGMA user_version = 73; COMMIT;"
        ))
        .map_err(StoreError::Migrate)
}

/// The wall clock the v74 step stamps its copied rows with.
fn migration_clock_ms() -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::MigrationClock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| StoreError::MigrationClock)
}

/// v73 -> v74: copy the retired consumer's gap evidence, mark every vehicle
/// with unpublished derived work in the export outbox, and drop the retired
/// objects. One transaction; an error rolls everything back (the transaction
/// is dropped without a commit).
fn migrate_retire_delivery_tables(connection: &Connection) -> Result<(), StoreError> {
    let copied_at_ms = migration_clock_ms()?;
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(StoreError::Migrate)?;
    retire_delivery_tables_in_transaction(&transaction, copied_at_ms)?;
    transaction.commit().map_err(StoreError::Migrate)
}

fn retire_delivery_tables_in_transaction(
    transaction: &Transaction<'_>,
    copied_at_ms: i64,
) -> Result<(), StoreError> {
    transaction
        .execute_batch(RETIRED_DELIVERY_GAPS_SQL)
        .map_err(StoreError::Migrate)?;
    let table_exists = |name: &str| -> Result<bool, StoreError> {
        transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
                params![name],
                |row| row.get(0),
            )
            .map_err(StoreError::Migrate)
    };
    let mut copied = 0usize;
    if table_exists("edge_sequence_dispositions")? {
        let has_states = table_exists("edge_accumulator_states")?;
        let vehicle_expression = if has_states {
            "(SELECT CASE WHEN COUNT(*) = 1 THEN MIN(s.vehicle_id) END
                FROM edge_accumulator_states AS s
               WHERE s.installation_id = d.installation_id
                 AND s.lineage = d.lineage)"
        } else {
            "NULL"
        };
        copied = transaction
            .execute(
                &format!(
                    "INSERT INTO retired_delivery_gaps(
                        installation_id, lineage, spool_seq, item_id, reason,
                        occurred_at_ms, evidence_sha256, committed_at_ms,
                        vehicle_id, copied_at_ms)
                     SELECT d.installation_id, d.lineage, d.spool_seq, d.item_id, d.reason,
                            d.occurred_at_ms, d.evidence_sha256, d.committed_at_ms,
                            {vehicle_expression}, ?1
                       FROM edge_sequence_dispositions AS d
                      WHERE d.category = 'durable_gap'
                      ORDER BY d.installation_id, d.lineage, d.spool_seq"
                ),
                params![copied_at_ms],
            )
            .map_err(StoreError::Migrate)?;
    }
    tracing::info!(target: "storage.migration", copied, "copied retired delivery gap rows");
    #[cfg(test)]
    retirement_fault_hit(RetirementFault::GapCopy).map_err(StoreError::Migrate)?;
    if table_exists("edge_pending_publications")? {
        let vehicles: Vec<String> = {
            let mut statement = transaction
                .prepare(
                    "SELECT DISTINCT vehicle_id FROM edge_pending_publications
                      WHERE status != 'complete' ORDER BY vehicle_id",
                )
                .map_err(StoreError::Migrate)?;
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(StoreError::Migrate)?
                .collect::<Result<_, _>>()
                .map_err(StoreError::Migrate)?
        };
        for vehicle in vehicles {
            // Everything but the clock fails the step as a migration error (R5).
            let vehicle_id = Uuid::parse_str(&vehicle).map_err(|error| {
                StoreError::Migrate(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                ))
            })?;
            mark_export_dirty_in_transaction(transaction, vehicle_id).map_err(|error| {
                match error {
                    StoreError::LifecycleWrite(error) => StoreError::Migrate(error),
                    other => other,
                }
            })?;
        }
    }
    #[cfg(test)]
    retirement_fault_hit(RetirementFault::OutboxMarking).map_err(StoreError::Migrate)?;
    transaction
        .execute_batch(RETIRED_DELIVERY_DROPS_SQL)
        .map_err(StoreError::Migrate)?;
    #[cfg(test)]
    retirement_fault_hit(RetirementFault::TableDrops).map_err(StoreError::Migrate)?;
    transaction
        .execute_batch("PRAGMA user_version = 74;")
        .map_err(StoreError::Migrate)?;
    Ok(())
}

impl HubStore {
    /// The stored complete accumulator state of one vehicle, if any.
    pub(crate) fn load_receiver_state(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT state_json FROM fleet_telemetry_accumulator_states
                  WHERE vehicle_id = ?1 AND state_version = 2",
                params![vehicle_id.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::ReceiverState)
    }

    /// Upsert one vehicle's complete accumulator state (version 2) inside the
    /// caller's transaction.
    pub(crate) fn put_receiver_state_in_transaction(
        transaction: &Transaction<'_>,
        vehicle_id: Uuid,
        state_json: &[u8],
        updated_at_ms: i64,
    ) -> Result<(), StoreError> {
        validate_timestamp("receiver state updated_at_ms", updated_at_ms)?;
        transaction
            .execute(
                "INSERT INTO fleet_telemetry_accumulator_states(
                    vehicle_id, state_version, state_json, updated_at_ms)
                 VALUES (?1, 2, ?2, ?3)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    state_version = 2,
                    state_json = excluded.state_json,
                    updated_at_ms = excluded.updated_at_ms",
                params![vehicle_id.to_string(), state_json, updated_at_ms],
            )
            .map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }
}
