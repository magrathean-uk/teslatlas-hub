// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialSlotFormatState {
    None,
    Pending,
    Migrated,
    Discarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialMigrationBlockReason {
    OldKeyUnavailable,
    OldPairInvalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialSecretsExclusion {
    Verified,
    Failed,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialDurabilityState {
    Ok,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct CredentialFormatState {
    pub(crate) owner_api_state: CredentialSlotFormatState,
    pub(crate) fleet_api_state: CredentialSlotFormatState,
    pub(crate) owner_api_blocked: Option<CredentialMigrationBlockReason>,
    pub(crate) fleet_api_blocked: Option<CredentialMigrationBlockReason>,
    pub(crate) fences_pending: bool,
    pub(crate) old_tables_dropped: bool,
    pub(crate) purge_required: bool,
    pub(crate) wal_scrubbed: bool,
    pub(crate) old_files_removed: bool,
    pub(crate) attempts_migrated: i64,
    pub(crate) fences_fingerprinted: i64,
    pub(crate) legacy_fences: i64,
    pub(crate) purge_needed_bytes: Option<i64>,
    pub(crate) secrets_exclusion: Option<CredentialSecretsExclusion>,
    pub(crate) durability: Option<CredentialDurabilityState>,
    pub(crate) durability_probed_at_ms: Option<i64>,
    pub(crate) started_at_ms: Option<i64>,
    pub(crate) converted_at_ms: Option<i64>,
    pub(crate) completed_at_ms: Option<i64>,
}

pub(crate) struct CredentialMigrationAttempt {
    pub(crate) receipt_id: OutboundRequestReceiptId,
    pub(crate) attempt_id: uuid::Uuid,
    pub(crate) slot: crate::auth::credential_envelope::CredentialSlot,
    pub(crate) input_generation: uuid::Uuid,
    pub(crate) output_generation: Option<uuid::Uuid>,
}

pub(crate) struct CredentialMigrationLegacyFence {
    pub(crate) generation: uuid::Uuid,
    pub(crate) identity: LegacyFenceIdentity,
    pub(crate) fenced_at_ms: i64,
}

pub(crate) struct CredentialMigrationRefreshFence {
    pub(crate) fingerprint: [u8; 32],
    pub(crate) fenced_at_ms: i64,
}

pub(crate) struct CredentialMigrationPlan<'a> {
    pub(crate) pairs: &'a [SealedCredentialPair],
    pub(crate) blocked_slots: &'a [(
        crate::auth::credential_envelope::CredentialSlot,
        CredentialMigrationBlockReason,
    )],
    pub(crate) attempts: &'a [CredentialMigrationAttempt],
    pub(crate) legacy_fences: &'a [CredentialMigrationLegacyFence],
    pub(crate) refresh_fences: &'a [CredentialMigrationRefreshFence],
    pub(crate) now_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CredentialMigrationCounts {
    pub(crate) attempts_migrated: i64,
    pub(crate) fences_fingerprinted: i64,
    pub(crate) legacy_fences: i64,
}

/// Per-open capabilities; none can be supplied by a migration caller.
#[derive(Default)]
struct CredentialMigrationRuntime {
    conversion_barrier_passed: bool,
    vacuum_succeeded: bool,
    purge_barrier_passed: bool,
    barrier_failed: bool,
}

impl CredentialFormatState {
    fn slot(
        &self,
        slot: CredentialSlot,
    ) -> (
        CredentialSlotFormatState,
        Option<CredentialMigrationBlockReason>,
    ) {
        match slot {
            CredentialSlot::OwnerApi => (self.owner_api_state, self.owner_api_blocked),
            CredentialSlot::FleetApi => (self.fleet_api_state, self.fleet_api_blocked),
        }
    }

    fn counts(&self) -> CredentialMigrationCounts {
        CredentialMigrationCounts {
            attempts_migrated: self.attempts_migrated,
            fences_fingerprinted: self.fences_fingerprinted,
            legacy_fences: self.legacy_fences,
        }
    }
}

impl CredentialSlotFormatState {
    fn from_sql_code(value: i64) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Pending),
            2 => Some(Self::Migrated),
            3 => Some(Self::Discarded),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::Migrated => "migrated",
            Self::Discarded => "discarded",
        }
    }
}

impl CredentialMigrationBlockReason {
    fn from_sql_code(value: i64) -> Option<Self> {
        match value {
            0 => Some(Self::OldKeyUnavailable),
            1 => Some(Self::OldPairInvalid),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::OldKeyUnavailable => "old_key_unavailable",
            Self::OldPairInvalid => "old_pair_invalid",
        }
    }
}

fn credential_migration_bool(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<bool> {
    match row.get::<_, i64>(index)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

#[cfg(test)]
std::thread_local! {
    static CREDENTIAL_FORMAT_STATE_QUERIES: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
    static CREDENTIAL_FORMAT_STATE_ENUM_BYTES: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
fn reset_test_credential_format_state_queries() {
    CREDENTIAL_FORMAT_STATE_QUERIES.with(|count| count.set(0));
}

#[cfg(test)]
fn test_credential_format_state_queries() -> usize {
    CREDENTIAL_FORMAT_STATE_QUERIES.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_test_credential_format_state_enum_bytes() {
    CREDENTIAL_FORMAT_STATE_ENUM_BYTES.with(|count| count.set(0));
}

#[cfg(test)]
fn test_credential_format_state_enum_bytes() -> usize {
    CREDENTIAL_FORMAT_STATE_ENUM_BYTES.with(std::cell::Cell::get)
}

/// Decodes exactly one state singleton. It deliberately does not recurse into
/// predecessor validation; the reader and write transactions add that check.
fn credential_format_state_on(
    connection: &Connection,
) -> Result<CredentialFormatState, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT singleton_id, format_version,
             CASE typeof(owner_api_state) WHEN 'text' THEN
                 CASE owner_api_state COLLATE BINARY WHEN 'none' THEN 0
                     WHEN 'pending' THEN 1 WHEN 'migrated' THEN 2
                     WHEN 'discarded' THEN 3 ELSE -1 END
                 ELSE -1 END,
             CASE typeof(fleet_api_state) WHEN 'text' THEN
                 CASE fleet_api_state COLLATE BINARY WHEN 'none' THEN 0
                     WHEN 'pending' THEN 1 WHEN 'migrated' THEN 2
                     WHEN 'discarded' THEN 3 ELSE -1 END
                 ELSE -1 END,
             CASE typeof(owner_api_blocked) WHEN 'null' THEN NULL WHEN 'text' THEN
                 CASE owner_api_blocked COLLATE BINARY WHEN 'old_key_unavailable' THEN 0
                     WHEN 'old_pair_invalid' THEN 1 ELSE -1 END
                 ELSE -1 END,
             CASE typeof(fleet_api_blocked) WHEN 'null' THEN NULL WHEN 'text' THEN
                 CASE fleet_api_blocked COLLATE BINARY WHEN 'old_key_unavailable' THEN 0
                     WHEN 'old_pair_invalid' THEN 1 ELSE -1 END
                 ELSE -1 END,
             fences_pending, old_tables_dropped,
             purge_required, wal_scrubbed, old_files_removed, attempts_migrated,
             fences_fingerprinted, legacy_fences, purge_needed_bytes,
             CASE typeof(secrets_exclusion) WHEN 'null' THEN NULL WHEN 'text' THEN
                 CASE secrets_exclusion COLLATE BINARY WHEN 'verified' THEN 0
                     WHEN 'failed' THEN 1 WHEN 'not_applicable' THEN 2 ELSE -1 END
                 ELSE -1 END,
             CASE typeof(durability) WHEN 'null' THEN NULL WHEN 'text' THEN
                 CASE durability COLLATE BINARY WHEN 'ok' THEN 0
                     WHEN 'unavailable' THEN 1 ELSE -1 END
                 ELSE -1 END,
             durability_probed_at_ms, started_at_ms, converted_at_ms,
             completed_at_ms FROM credential_format_state LIMIT 2",
        )
        .map_err(StoreError::CredentialPairStore)?;
    #[cfg(test)]
    CREDENTIAL_FORMAT_STATE_QUERIES.with(|count| count.set(count.get() + 1));
    let mut rows = statement
        .query([])
        .map_err(StoreError::CredentialPairStore)?;
    let row = rows
        .next()
        .map_err(StoreError::CredentialPairStore)?
        .ok_or(StoreError::CredentialPairInvalid)?;
    #[cfg(test)]
    for index in [2, 3, 4, 5, 15, 16] {
        let bytes = match row
            .get_ref(index)
            .map_err(StoreError::CredentialPairStore)?
        {
            rusqlite::types::ValueRef::Text(value) | rusqlite::types::ValueRef::Blob(value) => {
                value.len()
            }
            _ => 0,
        };
        CREDENTIAL_FORMAT_STATE_ENUM_BYTES
            .with(|count| count.set(count.get().saturating_add(bytes)));
    }
    let state = (|| -> rusqlite::Result<CredentialFormatState> {
        if row.get::<_, i64>(0)? != 1 || row.get::<_, i64>(1)? != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let parse_block = |index| -> rusqlite::Result<Option<CredentialMigrationBlockReason>> {
            row.get::<_, Option<i64>>(index)?
                .map(|value| {
                    CredentialMigrationBlockReason::from_sql_code(value)
                        .ok_or(rusqlite::Error::InvalidQuery)
                })
                .transpose()
        };
        let exclusion = row
            .get::<_, Option<i64>>(15)?
            .map(|value| match value {
                0 => Ok(CredentialSecretsExclusion::Verified),
                1 => Ok(CredentialSecretsExclusion::Failed),
                2 => Ok(CredentialSecretsExclusion::NotApplicable),
                _ => Err(rusqlite::Error::InvalidQuery),
            })
            .transpose()?;
        let durability = row
            .get::<_, Option<i64>>(16)?
            .map(|value| match value {
                0 => Ok(CredentialDurabilityState::Ok),
                1 => Ok(CredentialDurabilityState::Unavailable),
                _ => Err(rusqlite::Error::InvalidQuery),
            })
            .transpose()?;
        Ok(CredentialFormatState {
            owner_api_state: CredentialSlotFormatState::from_sql_code(row.get::<_, i64>(2)?)
                .ok_or(rusqlite::Error::InvalidQuery)?,
            fleet_api_state: CredentialSlotFormatState::from_sql_code(row.get::<_, i64>(3)?)
                .ok_or(rusqlite::Error::InvalidQuery)?,
            owner_api_blocked: parse_block(4)?,
            fleet_api_blocked: parse_block(5)?,
            fences_pending: credential_migration_bool(row, 6)?,
            old_tables_dropped: credential_migration_bool(row, 7)?,
            purge_required: credential_migration_bool(row, 8)?,
            wal_scrubbed: credential_migration_bool(row, 9)?,
            old_files_removed: credential_migration_bool(row, 10)?,
            attempts_migrated: row.get(11)?,
            fences_fingerprinted: row.get(12)?,
            legacy_fences: row.get(13)?,
            purge_needed_bytes: row.get(14)?,
            secrets_exclusion: exclusion,
            durability,
            durability_probed_at_ms: row.get(17)?,
            started_at_ms: row.get(18)?,
            converted_at_ms: row.get(19)?,
            completed_at_ms: row.get(20)?,
        })
    })()
    .map_err(|_| StoreError::CredentialPairInvalid)?;
    if rows
        .next()
        .map_err(StoreError::CredentialPairStore)?
        .is_some()
    {
        return Err(StoreError::CredentialPairInvalid);
    }
    let resolved = state.owner_api_state != CredentialSlotFormatState::Pending
        && state.fleet_api_state != CredentialSlotFormatState::Pending
        && !state.fences_pending
        && state.old_tables_dropped;
    let capacity = MAX_CREDENTIAL_REFRESH_FENCES as i64;
    if (state.owner_api_blocked.is_some()
        && state.owner_api_state != CredentialSlotFormatState::Pending)
        || (state.fleet_api_blocked.is_some()
            && state.fleet_api_state != CredentialSlotFormatState::Pending)
        || state.converted_at_ms.is_some() != resolved
        || (state.wal_scrubbed && state.purge_required)
        || (state.old_files_removed && state.converted_at_ms.is_none())
        || !(0..=capacity).contains(&state.attempts_migrated)
        || !(0..=capacity).contains(&state.legacy_fences)
        || !(0..=capacity).contains(&state.fences_fingerprinted)
        || state.legacy_fences + state.fences_fingerprinted > capacity
        || [
            state.purge_needed_bytes,
            state.durability_probed_at_ms,
            state.started_at_ms,
            state.converted_at_ms,
            state.completed_at_ms,
        ]
        .into_iter()
        .flatten()
        .any(|value| value < 0)
        || (state.completed_at_ms.is_some()
            && (!resolved
                || state.purge_required
                || !state.wal_scrubbed
                || !state.old_files_removed))
    {
        return Err(StoreError::CredentialPairInvalid);
    }
    Ok(state)
}

fn credential_migration_state_on(
    connection: &Connection,
) -> Result<CredentialFormatState, StoreError> {
    let version = schema_version(connection)?;
    if version != CREDENTIAL_FORMAT_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema(version));
    }
    let state = credential_format_state_on(connection)?;
    credential_pre_upgrade_validate(connection, &state)?;
    Ok(state)
}

impl HubStore {
    pub(crate) fn credential_opaque_fence_count(&self) -> Result<u64, StoreError> {
        let connection = self.open_read_only_connection()?;
        let version = schema_version(&connection)?;
        if version < CREDENTIAL_FORMAT_SCHEMA_VERSION { return Ok(0); }
        if version != CREDENTIAL_FORMAT_SCHEMA_VERSION { return Err(StoreError::UnsupportedSchema(version)); }
        connection.query_row(
            "SELECT COUNT(*) FROM credential_legacy_fences WHERE identity = 'opaque'",
            [],
            |row| row.get::<_, i64>(0),
        ).map(|count| count as u64).map_err(StoreError::Query)
    }

    pub(crate) fn credential_format_state(
        &self,
    ) -> Result<Option<CredentialFormatState>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let transaction = Transaction::new_unchecked(&connection, TransactionBehavior::Deferred)
            .map_err(StoreError::Begin)?;
        let version = schema_version(&transaction)?;
        if version < CREDENTIAL_FORMAT_SCHEMA_VERSION {
            return Ok(None);
        }
        if version != CREDENTIAL_FORMAT_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema(version));
        }
        let state = credential_format_state_on(&transaction)?;
        credential_pre_upgrade_validate(&transaction, &state)?;
        Ok(Some(state))
    }
}

impl CredentialConnection {
    pub(crate) fn credential_format_state(&mut self) -> Result<CredentialFormatState, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(StoreError::Begin)?;
        credential_migration_state_on(&transaction)
    }

    fn credential_migration_barrier(&mut self) -> Result<(), StoreError> {
        if self.migration_state.barrier_failed {
            return Err(StoreError::CredentialPairInvalid);
        }
        if let Err(error) = self.credential_durability_barrier() {
            self.migration_state.barrier_failed = true;
            self.record_unavailable_after_barrier_failure();
            return Err(error);
        }
        Ok(())
    }

    fn credential_migration_converted(&mut self) -> Result<CredentialFormatState, StoreError> {
        let state = credential_migration_state_on(&self.connection)?;
        if state.converted_at_ms.is_none() || self.migration_state.barrier_failed {
            return Err(StoreError::CredentialPairInvalid);
        }
        if !self.migration_state.conversion_barrier_passed {
            self.credential_migration_barrier()?;
            self.migration_state.conversion_barrier_passed = true;
        }
        Ok(state)
    }

    pub(crate) fn record_credential_secrets_exclusion(
        &mut self,
        state: CredentialSecretsExclusion,
    ) -> Result<(), StoreError> {
        if self.migration_state.barrier_failed {
            return Err(StoreError::CredentialPairInvalid);
        }
        let current = credential_migration_state_on(&self.connection)?;
        if current.secrets_exclusion == Some(state) {
            return Ok(());
        }
        let value = match state {
            CredentialSecretsExclusion::Verified => "verified",
            CredentialSecretsExclusion::Failed => "failed",
            CredentialSecretsExclusion::NotApplicable => "not_applicable",
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        credential_migration_state_on(&transaction)?;
        transaction
            .execute(
                "UPDATE credential_format_state SET secrets_exclusion=?1 WHERE singleton_id=1",
                [value],
            )
            .map_err(StoreError::CredentialPairStore)?;
        transaction
            .commit()
            .map_err(StoreError::CredentialPairStore)?;
        self.credential_migration_barrier()
    }

    pub(crate) fn defer_credential_purge(&mut self, needed_bytes: u64) -> Result<(), StoreError> {
        let needed = i64::try_from(needed_bytes).map_err(|_| StoreError::CredentialPairInvalid)?;
        let state = credential_migration_state_on(&self.connection)?;
        // M2P-59 never reverses a clean live store, even when the caller repeats a
        // capacity check using an obsolete shortfall.
        if state.converted_at_ms.is_some() && !state.purge_required {
            return Ok(());
        }
        if state.converted_at_ms.is_none() {
            return Err(StoreError::CredentialPairInvalid);
        }
        self.credential_migration_converted()?;
        if state.purge_needed_bytes == Some(needed) {
            return Ok(());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current = credential_migration_state_on(&transaction)?;
        if current.converted_at_ms.is_none() || !current.purge_required {
            return Err(StoreError::CredentialPairInvalid);
        }
        transaction
            .execute(
                "UPDATE credential_format_state SET purge_needed_bytes=?1 WHERE singleton_id=1",
                [needed],
            )
            .map_err(StoreError::CredentialPairStore)?;
        transaction
            .commit()
            .map_err(StoreError::CredentialPairStore)?;
        self.credential_migration_barrier()
    }

    pub(crate) fn vacuum_credential_catalogue(&mut self) -> Result<(), StoreError> {
        let state = credential_migration_state_on(&self.connection)?;
        if state.converted_at_ms.is_some() && !state.purge_required {
            return Ok(());
        }
        self.credential_migration_converted()?;
        self.migration_state.vacuum_succeeded = false;
        self.connection
            .execute_batch("VACUUM")
            .map_err(StoreError::CredentialPairStore)?;
        self.migration_state.vacuum_succeeded = true;
        Ok(())
    }

    pub(crate) fn commit_credential_purge(
        &mut self,
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        let state = credential_migration_state_on(&self.connection)?;
        if state.converted_at_ms.is_some() && !state.purge_required {
            return Ok(());
        }
        if !self.migration_state.vacuum_succeeded || self.migration_state.barrier_failed {
            return Err(StoreError::CredentialPairInvalid);
        }
        self.credential_migration_converted()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current = credential_migration_state_on(&transaction)?;
        if current.converted_at_ms.is_none() || !current.purge_required {
            return Err(StoreError::CredentialPairInvalid);
        }
        transaction
            .execute(
                "UPDATE credential_format_state SET purge_required=0,
                 purge_needed_bytes=NULL, wal_scrubbed=0 WHERE singleton_id=1",
                [],
            )
            .map_err(StoreError::CredentialPairStore)?;
        // R55 counts the COMMIT attempt, including a deferred-constraint error.
        before_commit();
        transaction
            .commit()
            .map_err(StoreError::CredentialPairStore)?;
        self.migration_state.vacuum_succeeded = false;
        self.migration_state.purge_barrier_passed = false;
        self.credential_migration_barrier()?;
        self.migration_state.purge_barrier_passed = true;
        Ok(())
    }

    pub(crate) fn mark_credential_wal_scrubbed(&mut self) -> Result<(), StoreError> {
        if self.migration_state.barrier_failed {
            return Err(StoreError::CredentialPairInvalid);
        }
        let state = credential_migration_state_on(&self.connection)?;
        if state.wal_scrubbed {
            return Ok(());
        }
        if state.converted_at_ms.is_none() || state.purge_required {
            return Err(StoreError::CredentialPairInvalid);
        }
        self.credential_migration_converted()?;
        if !self.migration_state.purge_barrier_passed {
            self.credential_migration_barrier()?;
            self.migration_state.purge_barrier_passed = true;
        }
        if !self.scrub_credential_wal()? {
            return Err(StoreError::CredentialPairInvalid);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current = credential_migration_state_on(&transaction)?;
        if current.converted_at_ms.is_none() || current.purge_required {
            return Err(StoreError::CredentialPairInvalid);
        }
        transaction
            .execute(
                "UPDATE credential_format_state SET wal_scrubbed=1 WHERE singleton_id=1",
                [],
            )
            .map_err(StoreError::CredentialPairStore)?;
        transaction
            .commit()
            .map_err(StoreError::CredentialPairStore)?;
        self.credential_migration_barrier()
    }

    pub(crate) fn mark_credential_files_removed(&mut self) -> Result<(), StoreError> {
        if self.migration_state.barrier_failed {
            return Err(StoreError::CredentialPairInvalid);
        }
        let state = credential_migration_state_on(&self.connection)?;
        if state.old_files_removed {
            return Ok(());
        }
        self.credential_migration_converted()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current = credential_migration_state_on(&transaction)?;
        if current.converted_at_ms.is_none() {
            return Err(StoreError::CredentialPairInvalid);
        }
        transaction
            .execute(
                "UPDATE credential_format_state SET old_files_removed=1 WHERE singleton_id=1",
                [],
            )
            .map_err(StoreError::CredentialPairStore)?;
        transaction
            .commit()
            .map_err(StoreError::CredentialPairStore)?;
        self.credential_migration_barrier()
    }

    pub(crate) fn mark_credential_migration_complete(
        &mut self,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        if self.migration_state.barrier_failed {
            return Err(StoreError::CredentialPairInvalid);
        }
        if now_ms < 0 {
            return Err(StoreError::CredentialPairInvalid);
        }
        let state = credential_migration_state_on(&self.connection)?;
        if state.completed_at_ms.is_some() {
            return Ok(());
        }
        if state.converted_at_ms.is_none()
            || state.purge_required
            || !state.wal_scrubbed
            || !state.old_files_removed
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        self.credential_migration_converted()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current = credential_migration_state_on(&transaction)?;
        if current.converted_at_ms.is_none()
            || current.purge_required
            || !current.wal_scrubbed
            || !current.old_files_removed
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        transaction
            .execute(
                "UPDATE credential_format_state SET completed_at_ms=?1
                 WHERE singleton_id=1 AND completed_at_ms IS NULL",
                [now_ms],
            )
            .map_err(StoreError::CredentialPairStore)?;
        transaction
            .commit()
            .map_err(StoreError::CredentialPairStore)?;
        self.credential_migration_barrier()
    }
}

fn credential_migration_check_slot_write(
    connection: &Connection,
    slot: CredentialSlot,
) -> Result<bool, StoreError> {
    let state = credential_migration_state_on(connection)?;
    let (format, reason) = state.slot(slot);
    if state.fences_pending || (format == CredentialSlotFormatState::Pending && reason.is_none()) {
        return Err(StoreError::CredentialPairInvalid);
    }
    Ok(format == CredentialSlotFormatState::Pending)
}

fn credential_migration_discard_blocked_slot(
    connection: &Connection,
    slot: CredentialSlot,
    now_ms: i64,
) -> Result<(), StoreError> {
    let state = credential_migration_state_on(connection)?;
    let (format, reason) = state.slot(slot);
    if format != CredentialSlotFormatState::Pending {
        return Ok(());
    }
    if now_ms < 0 || reason.is_none() || state.fences_pending {
        return Err(StoreError::CredentialPairInvalid);
    }
    credential_migration_require_mapped_history(connection)?;
    let table = match slot {
        CredentialSlot::OwnerApi => "legacy_owner_tokens",
        CredentialSlot::FleetApi => "fleet_tokens",
    };
    let deleted = connection
        .execute(&format!("DELETE FROM {table} WHERE singleton_id=1"), [])
        .map_err(StoreError::CredentialPairStore)?;
    if deleted != 1 {
        return Err(StoreError::CredentialPairInvalid);
    }
    let mut target = CredentialMigrationTarget::from_state(&state);
    target.set_slot(slot, CredentialSlotFormatState::Discarded, None);
    // R48 makes only the slot discard part of this transaction. Ordinary
    // Phase B subsequently verifies the mapped history and performs step 5's
    // table drop/conversion through the empty-plan action.
    let changed = connection
        .execute(
            "UPDATE credential_format_state SET owner_api_state=?1, fleet_api_state=?2,
             owner_api_blocked=?3, fleet_api_blocked=?4,
             started_at_ms=COALESCE(started_at_ms,?5) WHERE singleton_id=1",
            params![
                target.owner.as_str(),
                target.fleet.as_str(),
                target
                    .owner_block
                    .map(CredentialMigrationBlockReason::as_str),
                target
                    .fleet_block
                    .map(CredentialMigrationBlockReason::as_str),
                now_ms,
            ],
        )
        .map_err(StoreError::CredentialPairStore)?;
    if changed != 1 {
        return Err(StoreError::CredentialPairInvalid);
    }
    Ok(())
}

include!("credential_migration/conversion.rs");
include!("credential_migration/readers.rs");
