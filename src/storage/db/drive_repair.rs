// SPDX-License-Identifier: AGPL-3.0-only

// Imported broken-drive repair records (M03-T26): the candidate index an
// import captures, durable edit protection and unedited attestations, planned
// components with their members, undo suppressions and plan batches. Planning
// writes only these tables; no materialised row, pack or head changes here.
//
// The captured evidence lives in the catalogue itself, so every backup and
// restore carries it with the rows it describes; no external capture file is
// referenced and none needs pinning against pruning.

use crate::import::broken_drive_repair::{
    BrokenDriveCandidateIndex, CANDIDATE_PAGE_ROWS, ComponentPlanner, MemberRole, PlanCounts,
    PlanOutcome, REPAIR_POLICY_VERSION, RepairEligibility, RepairInput, RepairRefusal,
};

/// The version whose rung adds the drive repair tables. Main already assigns
/// v81 (import-state compaction) and v82 (collector retry columns), so this
/// rung takes the next free version and runs after every earlier rung.
const DRIVE_REPAIR_SCHEMA_VERSION: i32 = 83;
const _: () = assert!(
    DRIVE_REPAIR_SCHEMA_VERSION > SEAM_SCHEMA_VERSION + 2
        && DRIVE_REPAIR_SCHEMA_VERSION == SCHEMA_VERSION
);

const DRIVE_REPAIR_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS drive_repair_index_heads (
    vehicle_id TEXT PRIMARY KEY NOT NULL,
    source_id TEXT NOT NULL,
    car_id INTEGER NOT NULL CHECK(car_id > 0),
    capture_digest BLOB NOT NULL CHECK(length(capture_digest) = 32),
    policy_version INTEGER NOT NULL CHECK(policy_version > 0),
    row_count INTEGER NOT NULL CHECK(row_count >= 0),
    recorded_at_ms INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS drive_repair_inputs (
    vehicle_id TEXT NOT NULL,
    drive_id INTEGER NOT NULL CHECK(drive_id > 0),
    car_id INTEGER NOT NULL CHECK(car_id > 0),
    start_ms INTEGER NOT NULL,
    evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
    input_digest BLOB NOT NULL CHECK(length(input_digest) = 32),
    eligibility_revision INTEGER NOT NULL CHECK(eligibility_revision > 0),
    PRIMARY KEY(vehicle_id, drive_id)
) STRICT;
CREATE INDEX IF NOT EXISTS drive_repair_inputs_order
    ON drive_repair_inputs(vehicle_id, start_ms, drive_id);
CREATE TABLE IF NOT EXISTS drive_edit_protection (
    vehicle_id TEXT NOT NULL,
    entity TEXT NOT NULL CHECK(entity IN ('drive', 'position')),
    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
    revision INTEGER NOT NULL CHECK(revision > 0),
    reason TEXT NOT NULL CHECK(reason IN (
        'edit', 'delete', 'merge', 'split', 'annotation', 'protect', 'attested_unedited'
    )),
    attested_digest BLOB CHECK(attested_digest IS NULL OR length(attested_digest) = 32),
    recorded_at_ms INTEGER NOT NULL,
    CHECK((reason = 'attested_unedited') = (attested_digest IS NOT NULL)),
    PRIMARY KEY(vehicle_id, entity, entity_id)
) STRICT;
CREATE TABLE IF NOT EXISTS drive_repair_batches (
    batch_id INTEGER PRIMARY KEY,
    vehicle_id TEXT NOT NULL,
    policy_version INTEGER NOT NULL CHECK(policy_version > 0),
    state TEXT NOT NULL CHECK(state IN (
        'planned', 'staged', 'publishing', 'complete', 'failed', 'cancelled'
    )),
    due_at_ms INTEGER,
    cursor_start_ms INTEGER,
    cursor_drive_id INTEGER,
    totals_json TEXT NOT NULL CHECK(json_valid(totals_json)),
    expected_heads_json TEXT CHECK(expected_heads_json IS NULL OR json_valid(expected_heads_json)),
    staged_publication TEXT,
    capture_digest BLOB NOT NULL CHECK(length(capture_digest) = 32),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS drive_repair_changes (
    vehicle_id TEXT NOT NULL,
    component_key BLOB NOT NULL CHECK(length(component_key) = 32),
    batch_id INTEGER NOT NULL REFERENCES drive_repair_batches(batch_id),
    policy_version INTEGER NOT NULL CHECK(policy_version > 0),
    destination_drive_id INTEGER NOT NULL CHECK(destination_drive_id > 0),
    effective_start_ms INTEGER NOT NULL,
    input_digest BLOB NOT NULL CHECK(length(input_digest) = 32),
    protection_digest BLOB NOT NULL CHECK(length(protection_digest) = 32),
    before_json TEXT CHECK(before_json IS NULL OR json_valid(before_json)),
    after_json TEXT CHECK(after_json IS NULL OR json_valid(after_json)),
    state TEXT NOT NULL CHECK(state IN ('planned', 'active', 'undone', 'superseded')),
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY(vehicle_id, component_key)
) STRICT;
CREATE TABLE IF NOT EXISTS drive_repair_change_members (
    vehicle_id TEXT NOT NULL,
    entity TEXT NOT NULL CHECK(entity IN ('drive', 'position')),
    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
    component_key BLOB NOT NULL CHECK(length(component_key) = 32),
    PRIMARY KEY(vehicle_id, entity, entity_id, component_key)
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS drive_repair_suppressions (
    vehicle_id TEXT NOT NULL,
    component_key BLOB NOT NULL CHECK(length(component_key) = 32),
    reason TEXT NOT NULL CHECK(reason IN ('undo')),
    recorded_at_ms INTEGER NOT NULL,
    PRIMARY KEY(vehicle_id, component_key)
) STRICT, WITHOUT ROWID;";

/// The drive repair tables of this rung, for schema checks.
pub const DRIVE_REPAIR_TABLES: [&str; 7] = [
    "drive_repair_index_heads",
    "drive_repair_inputs",
    "drive_edit_protection",
    "drive_repair_batches",
    "drive_repair_changes",
    "drive_repair_change_members",
    "drive_repair_suppressions",
];

/// The versions this rung upgrades: every store the earlier rungs have
/// brought to v80 or later, below this rung. Called after every earlier rung,
/// so a store stamped by an earlier rung (v81 or v82 included) gains the
/// tables here whatever layout its earlier rungs left.
fn drive_repair_rung_applies(version: i32) -> bool {
    (SEAM_SCHEMA_VERSION..DRIVE_REPAIR_SCHEMA_VERSION).contains(&version)
}

/// Schema v83 (M03-T26): the drive repair tables, created where absent in one
/// transaction. A rung that stops part way rolls back and runs again.
fn drive_repair_schema_rung(connection: &Connection) -> Result<(), StoreError> {
    if !drive_repair_rung_applies(schema_version(connection)?) {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(StoreError::Migrate)?;
    admit_writable_catalogue(&transaction)?;
    if drive_repair_rung_applies(schema_version(&transaction)?) {
        transaction
            .execute_batch(DRIVE_REPAIR_SCHEMA)
            .map_err(StoreError::Migrate)?;
        transaction
            .pragma_update(None, "user_version", DRIVE_REPAIR_SCHEMA_VERSION)
            .map_err(StoreError::Migrate)?;
    }
    transaction.commit().map_err(StoreError::Migrate)
}

/// A human action that protects a whole repair component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveProtectionReason {
    Edit,
    Delete,
    Merge,
    Split,
    Annotation,
    Protect,
}

impl DriveProtectionReason {
    fn code(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Delete => "delete",
            Self::Merge => "merge",
            Self::Split => "split",
            Self::Annotation => "annotation",
            Self::Protect => "protect",
        }
    }
}

/// The entity a protection names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveRepairEntity {
    Drive,
    Position,
}

impl DriveRepairEntity {
    fn code(self) -> &'static str {
        match self {
            Self::Drive => "drive",
            Self::Position => "position",
        }
    }
}

/// What recording a captured index changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DriveRepairIndexReport {
    pub inserted: u64,
    pub changed: u64,
    pub unchanged: u64,
    pub removed: u64,
    pub superseded_components: u64,
}

/// One planning pass. Counts are separate from the import's own counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriveRepairPlanReport {
    pub batch_id: Option<i64>,
    pub index_present: bool,
    pub pages: u64,
    pub counts: PlanCounts,
    pub planned_keys: Vec<[u8; 32]>,
}

/// Aggregate record counts of one vehicle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DriveRepairSummary {
    pub index_rows: u64,
    pub planned: u64,
    pub active: u64,
    pub undone: u64,
    pub superseded: u64,
    pub suppressed: u64,
    pub protected: u64,
    pub attested: u64,
}

fn repair_count(transaction: &Connection, sql: &str, vehicle: &str) -> Result<u64, StoreError> {
    let count: i64 = transaction
        .query_row(sql, params![vehicle], |row| row.get(0))
        .map_err(StoreError::Query)?;
    u64::try_from(count).map_err(|_| StoreError::InvalidStoredCount)
}

fn supersede_components_of(
    transaction: &Transaction<'_>,
    vehicle: &str,
    entity: &str,
    entity_id: i64,
    now_ms: i64,
) -> Result<u64, StoreError> {
    let changed = transaction
        .execute(
            "UPDATE drive_repair_changes SET state = 'superseded', updated_at_ms = ?4
              WHERE vehicle_id = ?1 AND state IN ('planned', 'active')
                AND component_key IN (
                    SELECT component_key FROM drive_repair_change_members
                     WHERE vehicle_id = ?1 AND entity = ?2 AND entity_id = ?3)",
            params![vehicle, entity, entity_id, now_ms],
        )
        .map_err(StoreError::Query)?;
    Ok(changed as u64)
}

fn drop_attestation(
    transaction: &Transaction<'_>,
    vehicle: &str,
    drive_id: i64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "DELETE FROM drive_edit_protection
              WHERE vehicle_id = ?1 AND entity = 'drive' AND entity_id = ?2
                AND reason = 'attested_unedited'",
            params![vehicle, drive_id],
        )
        .map_err(StoreError::Query)?;
    Ok(())
}

fn decode_repair_input(json: &str, digest: &[u8]) -> Result<RepairInput, StoreError> {
    let input: RepairInput = serde_json::from_str(json)
        .map_err(|_| StoreError::DriveRepairRecord("candidate evidence"))?;
    if input.input_digest().as_slice() != digest {
        return Err(StoreError::DriveRepairRecord("candidate digest"));
    }
    Ok(input)
}

/// The store's ownership, protection and suppression answers for one pass.
struct StoreRepairEligibility<'a> {
    transaction: &'a Transaction<'a>,
    vehicle: String,
    /// `None` when coverage cannot be read (a window still open): every
    /// component then waits.
    coverage: Option<Vec<(i64, i64)>>,
}

impl StoreRepairEligibility<'_> {
    fn exists(&self, sql: &str, id: i64) -> Result<bool, StoreError> {
        self.transaction
            .query_row(sql, params![self.vehicle, id], |row| row.get::<_, bool>(0))
            .map_err(StoreError::Query)
    }

    fn seam_linked(&self, entity: &str, id: i64) -> Result<bool, StoreError> {
        self.transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM import_seam_links
                  WHERE vehicle_id = ?1 AND entity = ?2 AND (loser_id = ?3 OR winner_id = ?3))",
                params![self.vehicle, entity, id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(StoreError::Query)
    }

    fn catalogued(&self, entity: &str, id: i64) -> Result<bool, StoreError> {
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM {current} WHERE vehicle_id = ?1 AND entity = ?2 AND entity_id = ?3)
                 OR EXISTS(SELECT 1 FROM {legacy} WHERE vehicle_id = ?1 AND entity = ?2 AND entity_id = ?3)",
            current = crate::db::import_table_names::import_projection_state_rows_table!(),
            legacy = crate::db::import_table_names::import_projection_rows_table!(),
        );
        self.transaction
            .query_row(&sql, params![self.vehicle, entity, id], |row| row.get::<_, bool>(0))
            .map_err(StoreError::Query)
    }

    /// The sample must be an import-owned retained sample.
    fn sample_owned(&self, position_id: i64, detached: bool) -> Result<bool, StoreError> {
        if hub_ids::is_hub_id(position_id) || self.seam_linked("position", position_id)? {
            return Ok(false);
        }
        let sql = if detached {
            "SELECT EXISTS(SELECT 1 FROM materialised_positions
              WHERE vehicle_id = ?1 AND position_id = ?2 AND drive_id IS NULL)"
        } else {
            "SELECT EXISTS(SELECT 1 FROM materialised_positions
              WHERE vehicle_id = ?1 AND position_id = ?2)"
        };
        Ok(self.exists(sql, position_id)? && self.catalogued("position", position_id)?)
    }

    fn ownership_refusal(
        &self,
        member: &RepairInput,
        role: MemberRole,
    ) -> Result<bool, StoreError> {
        let id = member.drive_id;
        if hub_ids::is_hub_id(id)
            || self.seam_linked("drive", id)?
            || self.exists(
                "SELECT EXISTS(SELECT 1 FROM lifecycle_open_rows
                  WHERE vehicle_id = ?1 AND domain = 'drive' AND source_row_id = ?2)",
                id,
            )?
        {
            return Ok(true);
        }
        match role {
            MemberRole::Fragment => {
                let Some(sample) = member.first_sample else {
                    return Ok(true);
                };
                Ok(!self.sample_owned(sample.position_id, true)?)
            }
            MemberRole::Destination => {
                if !self.exists(
                    "SELECT EXISTS(SELECT 1 FROM materialised_drives
                      WHERE vehicle_id = ?1 AND drive_id = ?2)",
                    id,
                )? || !self.catalogued("drive", id)?
                {
                    return Ok(true);
                }
                let sql = format!(
                    "SELECT EXISTS(SELECT 1 FROM materialised_positions AS m
                      WHERE m.vehicle_id = ?1 AND m.drive_id = ?2
                        AND (m.position_id >= ?3
                             OR EXISTS(SELECT 1 FROM import_seam_links AS l
                                        WHERE l.vehicle_id = ?1 AND l.entity = 'position'
                                          AND (l.loser_id = m.position_id OR l.winner_id = m.position_id))
                             OR (NOT EXISTS(SELECT 1 FROM {current} AS c
                                             WHERE c.vehicle_id = ?1 AND c.entity = 'position'
                                               AND c.entity_id = m.position_id)
                                 AND NOT EXISTS(SELECT 1 FROM {legacy} AS g
                                                 WHERE g.vehicle_id = ?1 AND g.entity = 'position'
                                                   AND g.entity_id = m.position_id))))",
                    current = crate::db::import_table_names::import_projection_state_rows_table!(),
                    legacy = crate::db::import_table_names::import_projection_rows_table!(),
                );
                let foreign: bool = self
                    .transaction
                    .query_row(&sql, params![self.vehicle, id, hub_ids::HUB_ID_BASE], |row| {
                        row.get(0)
                    })
                    .map_err(StoreError::Query)?;
                let owned_first = match member.first_sample {
                    Some(sample) => self.sample_owned(sample.position_id, false)?,
                    None => false,
                };
                Ok(foreign || !owned_first)
            }
        }
    }

    fn protection(&self, entity: &str, id: i64) -> Result<Option<(String, Option<Vec<u8>>)>, StoreError> {
        self.transaction
            .query_row(
                "SELECT reason, attested_digest FROM drive_edit_protection
                  WHERE vehicle_id = ?1 AND entity = ?2 AND entity_id = ?3",
                params![self.vehicle, entity, id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)
    }
}

impl RepairEligibility for StoreRepairEligibility<'_> {
    type Error = StoreError;

    fn member_refusal(
        &mut self,
        member: &RepairInput,
        role: MemberRole,
    ) -> Result<Option<RepairRefusal>, StoreError> {
        if self.ownership_refusal(member, role)? {
            return Ok(Some(RepairRefusal::NotImportOwned));
        }
        let Some(coverage) = self.coverage.as_ref() else {
            return Ok(Some(RepairRefusal::CollectionCoverage));
        };
        let start = member.start_ms;
        let end = [
            member.end_ms,
            member.last_sample.map(|sample| sample.date_ms),
            Some(start),
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(start);
        if coverage.iter().any(|&(from, to)| from <= end && start <= to) {
            return Ok(Some(RepairRefusal::CollectionCoverage));
        }
        if let Some(sample) = member.first_sample
            && self.protection("position", sample.position_id)?.is_some()
        {
            return Ok(Some(RepairRefusal::Protected));
        }
        // A protection on any sample of the destination protects the whole
        // component, not only one on its first sample.
        if matches!(role, MemberRole::Destination)
            && self.exists(
                "SELECT EXISTS(SELECT 1 FROM materialised_positions AS m
                   JOIN drive_edit_protection AS p
                     ON p.vehicle_id = m.vehicle_id AND p.entity = 'position'
                    AND p.entity_id = m.position_id
                  WHERE m.vehicle_id = ?1 AND m.drive_id = ?2)",
                member.drive_id,
            )?
        {
            return Ok(Some(RepairRefusal::Protected));
        }
        match self.protection("drive", member.drive_id)? {
            Some((reason, Some(digest))) if reason == "attested_unedited" => {
                if digest.as_slice() == member.input_digest().as_slice() {
                    Ok(None)
                } else {
                    Ok(Some(RepairRefusal::UnknownEditHistory))
                }
            }
            Some(_) => Ok(Some(RepairRefusal::Protected)),
            None => Ok(Some(RepairRefusal::UnknownEditHistory)),
        }
    }

    fn suppressed(&mut self, component_key: &[u8; 32]) -> Result<bool, StoreError> {
        self.transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM drive_repair_suppressions
                  WHERE vehicle_id = ?1 AND component_key = ?2)",
                params![self.vehicle, component_key.as_slice()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }
}

impl HubStore {
    /// Records the candidate index an import captured, replacing the
    /// vehicle's previous one. A row whose evidence changed, or that left the
    /// index (closed or deleted at the source), loses its unedited attestation
    /// and supersedes every planned or active component it belongs to.
    pub fn record_drive_repair_index(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
        index: &BrokenDriveCandidateIndex,
        now_ms: i64,
    ) -> Result<DriveRepairIndexReport, StoreError> {
        let vehicle = vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Query)?;
        let mut existing = HashMap::<i64, (Vec<u8>, i64)>::new();
        {
            let mut statement = transaction
                .prepare(
                    "SELECT drive_id, input_digest, eligibility_revision
                       FROM drive_repair_inputs WHERE vehicle_id = ?1",
                )
                .map_err(StoreError::Query)?;
            let rows = statement
                .query_map(params![vehicle], |row| {
                    Ok((row.get::<_, i64>(0)?, (row.get(1)?, row.get(2)?)))
                })
                .map_err(StoreError::Query)?;
            for row in rows {
                let (id, value) = row.map_err(StoreError::Query)?;
                existing.insert(id, value);
            }
        }
        let mut report = DriveRepairIndexReport::default();
        for row in &index.rows {
            let digest = row.input_digest();
            let revision = match existing.remove(&row.drive_id) {
                Some((old, _)) if old.as_slice() == digest.as_slice() => {
                    report.unchanged += 1;
                    continue;
                }
                Some((_, revision)) => {
                    report.changed += 1;
                    drop_attestation(&transaction, &vehicle, row.drive_id)?;
                    report.superseded_components +=
                        supersede_components_of(&transaction, &vehicle, "drive", row.drive_id, now_ms)?;
                    revision + 1
                }
                None => {
                    report.inserted += 1;
                    1
                }
            };
            let evidence = serde_json::to_string(row)
                .map_err(|_| StoreError::DriveRepairRecord("candidate evidence"))?;
            transaction
                .execute(
                    "INSERT INTO drive_repair_inputs(vehicle_id, drive_id, car_id, start_ms,
                         evidence_json, input_digest, eligibility_revision)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(vehicle_id, drive_id) DO UPDATE SET
                         car_id = excluded.car_id, start_ms = excluded.start_ms,
                         evidence_json = excluded.evidence_json,
                         input_digest = excluded.input_digest,
                         eligibility_revision = excluded.eligibility_revision",
                    params![
                        vehicle,
                        row.drive_id,
                        row.car_id,
                        row.start_ms,
                        evidence,
                        digest.as_slice(),
                        revision
                    ],
                )
                .map_err(StoreError::Query)?;
        }
        for drive_id in existing.into_keys() {
            report.removed += 1;
            drop_attestation(&transaction, &vehicle, drive_id)?;
            report.superseded_components +=
                supersede_components_of(&transaction, &vehicle, "drive", drive_id, now_ms)?;
            transaction
                .execute(
                    "DELETE FROM drive_repair_inputs WHERE vehicle_id = ?1 AND drive_id = ?2",
                    params![vehicle, drive_id],
                )
                .map_err(StoreError::Query)?;
        }
        transaction
            .execute(
                "INSERT INTO drive_repair_index_heads(vehicle_id, source_id, car_id,
                     capture_digest, policy_version, row_count, recorded_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(vehicle_id) DO UPDATE SET source_id = excluded.source_id,
                     car_id = excluded.car_id, capture_digest = excluded.capture_digest,
                     policy_version = excluded.policy_version, row_count = excluded.row_count,
                     recorded_at_ms = excluded.recorded_at_ms",
                params![
                    vehicle,
                    source_id.to_string(),
                    index.car_id,
                    index.capture_digest().as_slice(),
                    REPAIR_POLICY_VERSION,
                    index.rows.len() as i64,
                    now_ms
                ],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(report)
    }

    /// Withdraws the vehicle's recorded index before an import may commit.
    /// Planning needs the index head, so until the import records its own
    /// capture (`record_drive_repair_index`), nothing is planned from evidence
    /// an import may have made stale. Rows and attestations stay, so the next
    /// recording still compares each row with its previous evidence.
    pub fn invalidate_drive_repair_index(&self, vehicle_id: Uuid) -> Result<(), StoreError> {
        let connection = self.open()?;
        connection
            .execute(
                "DELETE FROM drive_repair_index_heads WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
            )
            .map_err(StoreError::Query)?;
        Ok(())
    }

    /// One page of the stored index in source start order, after `after`.
    pub fn drive_repair_index_page(
        &self,
        vehicle_id: Uuid,
        after: Option<(i64, i64)>,
        limit: usize,
    ) -> Result<Vec<RepairInput>, StoreError> {
        let connection = self.open_read_only_connection()?;
        drive_repair_index_page_in(&connection, &vehicle_id.to_string(), after, limit)
    }

    /// Records a human action that protects every component naming the entity.
    /// A protection replaces an attestation and supersedes planned components.
    pub fn record_drive_edit_protection(
        &self,
        vehicle_id: Uuid,
        entity: DriveRepairEntity,
        entity_id: i64,
        reason: DriveProtectionReason,
        now_ms: i64,
    ) -> Result<i64, StoreError> {
        let vehicle = vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Query)?;
        let revision: i64 = transaction
            .query_row(
                "INSERT INTO drive_edit_protection(vehicle_id, entity, entity_id, revision,
                     reason, attested_digest, recorded_at_ms)
                 VALUES (?1, ?2, ?3, 1, ?4, NULL, ?5)
                 ON CONFLICT(vehicle_id, entity, entity_id) DO UPDATE SET
                     revision = drive_edit_protection.revision + 1, reason = excluded.reason,
                     attested_digest = NULL, recorded_at_ms = excluded.recorded_at_ms
                 RETURNING revision",
                params![vehicle, entity.code(), entity_id, reason.code(), now_ms],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        supersede_components_of(&transaction, &vehicle, entity.code(), entity_id, now_ms)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(revision)
    }

    /// Records the owner's attestation that the selected indexed drives are
    /// unedited, bound to their current evidence digests. A protected or
    /// unindexed drive is skipped. Returns the number attested.
    pub fn attest_unedited_drive_repair_candidates(
        &self,
        vehicle_id: Uuid,
        drive_ids: &[i64],
        now_ms: i64,
    ) -> Result<u64, StoreError> {
        let vehicle = vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Query)?;
        let mut attested = 0_u64;
        for &drive_id in drive_ids {
            attested += transaction
                .execute(
                    "INSERT INTO drive_edit_protection(vehicle_id, entity, entity_id, revision,
                         reason, attested_digest, recorded_at_ms)
                     SELECT vehicle_id, 'drive', drive_id, 1, 'attested_unedited', input_digest, ?3
                       FROM drive_repair_inputs WHERE vehicle_id = ?1 AND drive_id = ?2
                     ON CONFLICT(vehicle_id, entity, entity_id) DO UPDATE SET
                         revision = drive_edit_protection.revision + 1,
                         attested_digest = excluded.attested_digest,
                         recorded_at_ms = excluded.recorded_at_ms
                     WHERE drive_edit_protection.reason = 'attested_unedited'",
                    params![vehicle, drive_id, now_ms],
                )
                .map_err(StoreError::Query)? as u64;
        }
        transaction.commit().map_err(StoreError::Query)?;
        Ok(attested)
    }

    /// Plans repairs from the stored index alone, page by page, and records
    /// the planned components. No served row, pack or head changes.
    pub fn plan_drive_repairs(
        &self,
        vehicle_id: Uuid,
        now_ms: i64,
    ) -> Result<DriveRepairPlanReport, StoreError> {
        let vehicle = vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Query)?;
        let capture_digest: Option<Vec<u8>> = transaction
            .query_row(
                "SELECT capture_digest FROM drive_repair_index_heads WHERE vehicle_id = ?1",
                params![vehicle],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some(capture_digest) = capture_digest else {
            return Ok(DriveRepairPlanReport::default());
        };
        let coverage = coverage_intervals_for_vehicle(&transaction, &vehicle).ok();
        transaction
            .execute(
                "INSERT INTO drive_repair_batches(vehicle_id, policy_version, state, totals_json,
                     capture_digest, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, 'planned', '{}', ?3, ?4, ?4)",
                params![vehicle, REPAIR_POLICY_VERSION, capture_digest, now_ms],
            )
            .map_err(StoreError::Query)?;
        let batch_id = transaction.last_insert_rowid();
        let mut eligibility = StoreRepairEligibility {
            transaction: &transaction,
            vehicle: vehicle.clone(),
            coverage,
        };
        let mut planner = ComponentPlanner::new(vehicle.clone(), now_ms);
        let mut report = DriveRepairPlanReport {
            batch_id: Some(batch_id),
            index_present: true,
            ..DriveRepairPlanReport::default()
        };
        let mut outcomes = Vec::new();
        let mut cursor = None;
        loop {
            let page = drive_repair_index_page_in(&transaction, &vehicle, cursor, CANDIDATE_PAGE_ROWS)?;
            if page.is_empty() {
                break;
            }
            report.pages += 1;
            let full = page.len() == CANDIDATE_PAGE_ROWS;
            for row in page {
                cursor = Some((row.start_ms, row.drive_id));
                outcomes.extend(planner.push(row, &mut eligibility)?);
            }
            if !full {
                break;
            }
        }
        let (counts, tail) = planner.finish();
        outcomes.extend(tail);
        for outcome in outcomes {
            let PlanOutcome::Planned(component) = outcome else {
                continue;
            };
            let protection_digest = protection_digest(&transaction, &vehicle, &component.members)?;
            transaction
                .execute(
                    "INSERT INTO drive_repair_changes(vehicle_id, component_key, batch_id,
                         policy_version, destination_drive_id, effective_start_ms, input_digest,
                         protection_digest, state, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'planned', ?9)
                     ON CONFLICT(vehicle_id, component_key) DO UPDATE SET
                         batch_id = excluded.batch_id, protection_digest = excluded.protection_digest,
                         updated_at_ms = excluded.updated_at_ms, state = 'planned'
                     WHERE drive_repair_changes.state IN ('planned', 'superseded', 'undone')",
                    params![
                        vehicle,
                        component.component_key.as_slice(),
                        batch_id,
                        REPAIR_POLICY_VERSION,
                        component.destination_drive_id,
                        component.effective_start_ms,
                        component.input_digest.as_slice(),
                        protection_digest.as_slice(),
                        now_ms
                    ],
                )
                .map_err(StoreError::Query)?;
            let members = component
                .fragment_drive_ids
                .iter()
                .chain(std::iter::once(&component.destination_drive_id))
                .map(|&id| ("drive", id))
                .chain(component.sample_position_ids.iter().map(|&id| ("position", id)));
            for (entity, id) in members {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO drive_repair_change_members(vehicle_id, entity,
                             entity_id, component_key) VALUES (?1, ?2, ?3, ?4)",
                        params![vehicle, entity, id, component.component_key.as_slice()],
                    )
                    .map_err(StoreError::Query)?;
            }
            // Every destination sample is a member too, so protecting any of
            // them later supersedes the component.
            transaction
                .execute(
                    "INSERT OR IGNORE INTO drive_repair_change_members(vehicle_id, entity,
                         entity_id, component_key)
                     SELECT vehicle_id, 'position', position_id, ?3 FROM materialised_positions
                      WHERE vehicle_id = ?1 AND drive_id = ?2",
                    params![
                        vehicle,
                        component.destination_drive_id,
                        component.component_key.as_slice()
                    ],
                )
                .map_err(StoreError::Query)?;
            report.planned_keys.push(component.component_key);
        }
        let totals = serde_json::to_string(&counts)
            .map_err(|_| StoreError::DriveRepairRecord("plan totals"))?;
        transaction
            .execute(
                "UPDATE drive_repair_batches SET totals_json = ?2, cursor_start_ms = ?3,
                     cursor_drive_id = ?4, updated_at_ms = ?5 WHERE batch_id = ?1",
                params![
                    batch_id,
                    totals,
                    cursor.map(|(start, _)| start),
                    cursor.map(|(_, id)| id),
                    now_ms
                ],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::Query)?;
        report.counts = counts;
        Ok(report)
    }

    /// Undoes one component's record and suppresses its identity until an
    /// explicit retry. Returns whether a planned or active record changed.
    pub fn undo_drive_repair_component(
        &self,
        vehicle_id: Uuid,
        component_key: &[u8; 32],
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let vehicle = vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Query)?;
        let changed = transaction
            .execute(
                "UPDATE drive_repair_changes SET state = 'undone', updated_at_ms = ?3
                  WHERE vehicle_id = ?1 AND component_key = ?2 AND state IN ('planned', 'active')",
                params![vehicle, component_key.as_slice(), now_ms],
            )
            .map_err(StoreError::Query)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO drive_repair_suppressions(vehicle_id, component_key,
                     reason, recorded_at_ms) VALUES (?1, ?2, 'undo', ?3)",
                params![vehicle, component_key.as_slice(), now_ms],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(changed == 1)
    }

    /// The explicit Retry repair action: removes a suppression so the next
    /// pass revalidates the component. Returns whether one was removed.
    pub fn retry_drive_repair_component(
        &self,
        vehicle_id: Uuid,
        component_key: &[u8; 32],
    ) -> Result<bool, StoreError> {
        let connection = self.open()?;
        let removed = connection
            .execute(
                "DELETE FROM drive_repair_suppressions WHERE vehicle_id = ?1 AND component_key = ?2",
                params![vehicle_id.to_string(), component_key.as_slice()],
            )
            .map_err(StoreError::Query)?;
        Ok(removed == 1)
    }

    /// Aggregate counts only; no sample value leaves the store.
    pub fn drive_repair_summary(&self, vehicle_id: Uuid) -> Result<DriveRepairSummary, StoreError> {
        let connection = self.open_read_only_connection()?;
        let vehicle = vehicle_id.to_string();
        let state = |state: &str| {
            repair_count(
                &connection,
                &format!(
                    "SELECT COUNT(*) FROM drive_repair_changes WHERE vehicle_id = ?1 AND state = '{state}'"
                ),
                &vehicle,
            )
        };
        Ok(DriveRepairSummary {
            index_rows: repair_count(
                &connection,
                "SELECT COUNT(*) FROM drive_repair_inputs WHERE vehicle_id = ?1",
                &vehicle,
            )?,
            planned: state("planned")?,
            active: state("active")?,
            undone: state("undone")?,
            superseded: state("superseded")?,
            suppressed: repair_count(
                &connection,
                "SELECT COUNT(*) FROM drive_repair_suppressions WHERE vehicle_id = ?1",
                &vehicle,
            )?,
            protected: repair_count(
                &connection,
                "SELECT COUNT(*) FROM drive_edit_protection
                  WHERE vehicle_id = ?1 AND reason <> 'attested_unedited'",
                &vehicle,
            )?,
            attested: repair_count(
                &connection,
                "SELECT COUNT(*) FROM drive_edit_protection
                  WHERE vehicle_id = ?1 AND reason = 'attested_unedited'",
                &vehicle,
            )?,
        })
    }
}

fn drive_repair_index_page_in(
    connection: &Connection,
    vehicle: &str,
    after: Option<(i64, i64)>,
    limit: usize,
) -> Result<Vec<RepairInput>, StoreError> {
    let (start, id) = after.unwrap_or((i64::MIN, i64::MIN));
    let mut statement = connection
        .prepare_cached(
            "SELECT evidence_json, input_digest FROM drive_repair_inputs
              WHERE vehicle_id = ?1 AND (start_ms, drive_id) > (?2, ?3)
              ORDER BY start_ms, drive_id LIMIT ?4",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(
            params![vehicle, start, id, limit.min(CANDIDATE_PAGE_ROWS) as i64],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .map_err(StoreError::Query)?;
    let mut page = Vec::new();
    for row in rows {
        let (json, digest) = row.map_err(StoreError::Query)?;
        page.push(decode_repair_input(&json, &digest)?);
    }
    Ok(page)
}

fn protection_digest(
    transaction: &Transaction<'_>,
    vehicle: &str,
    members: &[RepairInput],
) -> Result<[u8; 32], StoreError> {
    let mut hasher = Sha256::new();
    hasher.update(b"tesatlas-hub/drive-repair-protection/v2\0");
    for member in members {
        let revision: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(revision), 0) FROM drive_edit_protection
                  WHERE vehicle_id = ?1 AND entity = 'drive' AND entity_id = ?2",
                params![vehicle, member.drive_id],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        // The member's own sample and every sample materialised under it.
        let sample_revision: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(p.revision), 0) FROM drive_edit_protection AS p
                  WHERE p.vehicle_id = ?1 AND p.entity = 'position'
                    AND (p.entity_id = ?3 OR p.entity_id IN (
                        SELECT position_id FROM materialised_positions
                         WHERE vehicle_id = ?1 AND drive_id = ?2))",
                params![
                    vehicle,
                    member.drive_id,
                    member.first_sample.map(|sample| sample.position_id)
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        hasher.update(member.drive_id.to_be_bytes());
        hasher.update(revision.to_be_bytes());
        hasher.update(sample_revision.to_be_bytes());
    }
    Ok(hasher.finalize().into())
}
