// SPDX-License-Identifier: AGPL-3.0-only

#[derive(PartialEq, Eq)]
struct CredentialMigrationTarget {
    owner: CredentialSlotFormatState,
    fleet: CredentialSlotFormatState,
    owner_block: Option<CredentialMigrationBlockReason>,
    fleet_block: Option<CredentialMigrationBlockReason>,
    fences_pending: bool,
}

impl CredentialMigrationTarget {
    fn from_state(state: &CredentialFormatState) -> Self {
        Self {
            owner: state.owner_api_state,
            fleet: state.fleet_api_state,
            owner_block: state.owner_api_blocked,
            fleet_block: state.fleet_api_blocked,
            fences_pending: state.fences_pending,
        }
    }

    fn set_slot(
        &mut self,
        slot: CredentialSlot,
        state: CredentialSlotFormatState,
        blocked: Option<CredentialMigrationBlockReason>,
    ) {
        match slot {
            CredentialSlot::OwnerApi => {
                self.owner = state;
                self.owner_block = blocked;
            }
            CredentialSlot::FleetApi => {
                self.fleet = state;
                self.fleet_block = blocked;
            }
        }
    }
}

fn credential_migration_drop_predecessors(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute_batch(
            "DROP TABLE IF EXISTS legacy_refresh_receipt_bindings;
             DROP TABLE IF EXISTS legacy_refresh_input_fences;
             DROP TABLE IF EXISTS fleet_refresh_receipt_bindings;
             DROP TABLE IF EXISTS fleet_refresh_input_fences;
             DROP TABLE IF EXISTS legacy_owner_tokens;
             DROP TABLE IF EXISTS fleet_tokens;",
        )
        .map_err(StoreError::CredentialPairStore)
}

fn credential_migration_update_state(
    connection: &Connection,
    current: &CredentialFormatState,
    target: &CredentialMigrationTarget,
    counts: CredentialMigrationCounts,
    now_ms: i64,
) -> Result<(), StoreError> {
    let converted = target.owner != CredentialSlotFormatState::Pending
        && target.fleet != CredentialSlotFormatState::Pending
        && !target.fences_pending;
    if now_ms < 0 || (current.converted_at_ms.is_some() && !converted) {
        return Err(StoreError::CredentialPairInvalid);
    }
    if converted {
        credential_migration_require_mapped_history(connection)?;
        credential_migration_drop_predecessors(connection)?;
    }
    let changed = connection
        .execute(
            "UPDATE credential_format_state SET owner_api_state=?1, fleet_api_state=?2,
             owner_api_blocked=?3, fleet_api_blocked=?4, fences_pending=?5,
             attempts_migrated=?6, fences_fingerprinted=?7, legacy_fences=?8,
             started_at_ms=COALESCE(started_at_ms,?9),
             old_tables_dropped=CASE WHEN ?10 THEN 1 ELSE old_tables_dropped END,
             converted_at_ms=CASE WHEN ?10 THEN COALESCE(converted_at_ms,?9)
                                 ELSE converted_at_ms END WHERE singleton_id=1",
            params![
                target.owner.as_str(),
                target.fleet.as_str(),
                target
                    .owner_block
                    .map(CredentialMigrationBlockReason::as_str),
                target
                    .fleet_block
                    .map(CredentialMigrationBlockReason::as_str),
                target.fences_pending,
                counts.attempts_migrated,
                counts.fences_fingerprinted,
                counts.legacy_fences,
                now_ms,
                converted,
            ],
        )
        .map_err(StoreError::CredentialPairStore)?;
    if changed != 1 {
        return Err(StoreError::CredentialPairInvalid);
    }
    Ok(())
}

fn credential_migration_persisted_legacy(
    connection: &Connection,
) -> Result<std::collections::BTreeMap<Uuid, (LegacyFenceIdentity, i64)>, StoreError> {
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM credential_legacy_fences", [], |row| {
            row.get(0)
        })
        .map_err(StoreError::CredentialPairStore)?;
    if !(0..=MAX_CREDENTIAL_REFRESH_FENCES as i64).contains(&count) {
        return Err(StoreError::CredentialPairInvalid);
    }
    let mut statement = connection
        .prepare("SELECT legacy_generation, identity, fenced_at_ms FROM credential_legacy_fences")
        .map_err(StoreError::CredentialPairStore)?;
    let mut rows = statement
        .query([])
        .map_err(StoreError::CredentialPairStore)?;
    let mut result = std::collections::BTreeMap::new();
    while let Some(row) = rows.next().map_err(StoreError::CredentialPairStore)? {
        let raw: String = row.get(0).map_err(StoreError::CredentialPairStore)?;
        let generation = Uuid::parse_str(&raw).map_err(|_| StoreError::CredentialPairInvalid)?;
        let identity = match row
            .get::<_, String>(1)
            .map_err(StoreError::CredentialPairStore)?
            .as_str()
        {
            "anchored" => LegacyFenceIdentity::Anchored,
            "opaque" => LegacyFenceIdentity::Opaque,
            _ => return Err(StoreError::CredentialPairInvalid),
        };
        let time: i64 = row.get(2).map_err(StoreError::CredentialPairStore)?;
        if raw != generation.to_string()
            || time < 0
            || result.insert(generation, (identity, time)).is_some()
        {
            return Err(StoreError::CredentialPairInvalid);
        }
    }
    Ok(result)
}

/// A false fences_pending flag is not permission to discard source history.
/// This correspondence check never derives or resets historical totals.
fn credential_migration_require_mapped_history(connection: &Connection) -> Result<(), StoreError> {
    if !credential_pre_upgrade_presence(connection)? {
        return Ok(());
    }
    for (slot, table) in [
        (CredentialSlot::OwnerApi, "legacy_refresh_receipt_bindings"),
        (CredentialSlot::FleetApi, "fleet_refresh_receipt_bindings"),
    ] {
        let missing: bool = connection
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {table} AS old_binding
                     WHERE NOT EXISTS(SELECT 1 FROM credential_refresh_attempts AS migrated
                     WHERE migrated.receipt_id=old_binding.receipt_id AND migrated.slot=?1
                     AND migrated.migrated=1
                     AND migrated.input_generation=old_binding.input_credential_generation COLLATE NOCASE
                     AND ((migrated.output_generation IS NULL
                           AND old_binding.output_credential_generation IS NULL)
                          OR migrated.output_generation=old_binding.output_credential_generation COLLATE NOCASE)))"
                ),
                [slot.as_str()],
                |row| row.get(0),
            )
            .map_err(StoreError::CredentialPairStore)?;
        if missing {
            return Err(StoreError::CredentialPairInvalid);
        }
    }
    for table in ["legacy_refresh_input_fences", "fleet_refresh_input_fences"] {
        let missing: bool = connection
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {table} AS old_fence WHERE NOT EXISTS(
                     SELECT 1 FROM credential_legacy_fences AS migrated
                     WHERE migrated.legacy_generation=old_fence.input_credential_generation COLLATE NOCASE))"
                ),
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::CredentialPairStore)?;
        if missing {
            return Err(StoreError::CredentialPairInvalid);
        }
    }
    Ok(())
}

fn credential_migration_check_refresh_vector(
    supplied: &[CredentialMigrationRefreshFence],
    derived: &std::collections::BTreeMap<[u8; 32], i64>,
    allow_empty: bool,
) -> Result<(), StoreError> {
    if allow_empty && supplied.is_empty() {
        return Ok(());
    }
    if supplied.len() != derived.len() {
        return Err(StoreError::CredentialPairInvalid);
    }
    let mut seen = std::collections::BTreeSet::new();
    for item in supplied {
        if !seen.insert(item.fingerprint)
            || derived.get(&item.fingerprint) != Some(&item.fenced_at_ms)
        {
            return Err(StoreError::CredentialPairInvalid);
        }
    }
    Ok(())
}

fn credential_migration_insert_refresh_fences(
    connection: &Connection,
    derived: &std::collections::BTreeMap<[u8; 32], i64>,
) -> Result<i64, StoreError> {
    let mut inserted = 0;
    for (fingerprint, now_ms) in derived {
        let existing: Option<String> = connection
            .query_row(
                "SELECT kind FROM credential_refresh_fences WHERE fingerprint=?1",
                [fingerprint.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::CredentialPairStore)?;
        match existing.as_deref() {
            Some("migrated") => {}
            None => {
                connection
                    .execute(
                        "INSERT INTO credential_refresh_fences(fingerprint,kind,fenced_at_ms)
                         VALUES(?1,'migrated',?2)",
                        params![fingerprint.as_slice(), now_ms],
                    )
                    .map_err(StoreError::CredentialPairStore)?;
                inserted += 1;
            }
            _ => return Err(StoreError::CredentialPairInvalid),
        }
    }
    Ok(inserted)
}

fn credential_migration_check_capacity(
    connection: &Connection,
    capacity: usize,
) -> Result<(), StoreError> {
    let total: i64 = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM credential_legacy_fences)
                    +(SELECT COUNT(*) FROM credential_refresh_fences)",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::CredentialPairStore)?;
    if total < 0 || u64::try_from(total).unwrap_or(u64::MAX) > capacity as u64 {
        return Err(StoreError::CredentialPairInvalid);
    }
    Ok(())
}

/// E-A1 requires refusal before the first mutation, not merely rollback after
/// an over-cap insert. The derived map already deduplicates shared fingerprints;
/// promoting an existing legacy identity does not add another legacy row.
fn credential_migration_check_prospective_capacity(
    connection: &Connection,
    persisted: &std::collections::BTreeMap<Uuid, (LegacyFenceIdentity, i64)>,
    initial_legacy: Option<&std::collections::BTreeMap<Uuid, i64>>,
    derived: &std::collections::BTreeMap<[u8; 32], i64>,
    capacity: usize,
) -> Result<i64, StoreError> {
    let current_refresh: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM credential_refresh_fences",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::CredentialPairStore)?;
    let current_refresh =
        u64::try_from(current_refresh).map_err(|_| StoreError::CredentialPairInvalid)?;
    let additional_legacy = initial_legacy.map_or(0, |legacy| {
        legacy
            .keys()
            .filter(|generation| !persisted.contains_key(generation))
            .count()
    });
    let mut additional_refresh = 0_u64;
    for fingerprint in derived.keys() {
        let existing: Option<String> = connection
            .query_row(
                "SELECT kind FROM credential_refresh_fences WHERE fingerprint=?1",
                [fingerprint.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::CredentialPairStore)?;
        match existing.as_deref() {
            Some("migrated") => {}
            None => additional_refresh += 1,
            _ => return Err(StoreError::CredentialPairInvalid),
        }
    }
    let prospective = (persisted.len() as u64)
        .checked_add(additional_legacy as u64)
        .and_then(|count| count.checked_add(current_refresh))
        .and_then(|count| count.checked_add(additional_refresh))
        .ok_or(StoreError::CredentialPairInvalid)?;
    if prospective > capacity as u64 {
        return Err(StoreError::CredentialPairInvalid);
    }
    i64::try_from(additional_refresh).map_err(|_| StoreError::CredentialPairInvalid)
}

impl CredentialConnection {
    pub(crate) fn commit_credential_migration(
        &mut self,
        plan: &CredentialMigrationPlan<'_>,
        before_commit: &mut dyn FnMut(),
    ) -> Result<CredentialMigrationCounts, StoreError> {
        let capacity = self.capacity();
        let failed_barrier = self.migration_state.barrier_failed;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let state = credential_migration_state_on(&transaction)?;
        if plan.now_ms < 0
            || plan.pairs.len() > 2
            || plan.blocked_slots.len() > 2
            || plan.attempts.len() > MAX_OUTBOUND_REQUEST_RECEIPTS as usize
            || plan.legacy_fences.len() > MAX_CREDENTIAL_REFRESH_FENCES
            || plan.refresh_fences.len() > 2
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        if state.converted_at_ms.is_some() {
            if !plan.pairs.is_empty()
                || !plan.blocked_slots.is_empty()
                || !plan.attempts.is_empty()
                || !plan.legacy_fences.is_empty()
                || !plan.refresh_fences.is_empty()
            {
                return Err(StoreError::CredentialPairInvalid);
            }
            return Ok(state.counts());
        }
        if failed_barrier {
            return Err(StoreError::CredentialPairInvalid);
        }
        let initial = state.fences_pending;
        if !initial && (!plan.attempts.is_empty() || !plan.legacy_fences.is_empty()) {
            return Err(StoreError::CredentialPairInvalid);
        }
        let owner_bindings =
            credential_pre_upgrade_bindings_on(&transaction, CredentialSlot::OwnerApi)?;
        let fleet_bindings =
            credential_pre_upgrade_bindings_on(&transaction, CredentialSlot::FleetApi)?;
        let mut target = CredentialMigrationTarget::from_state(&state);
        let mut converted_slots = std::collections::BTreeSet::new();
        for pair in plan.pairs {
            pair.validate()?;
            if !pair.migrated
                || !pair.refresh_present
                || pair.refresh_fingerprint.is_none()
                || (pair.slot == CredentialSlot::OwnerApi && pair.origin.is_some())
                || state.slot(pair.slot).0 != CredentialSlotFormatState::Pending
                || !converted_slots.insert(pair.slot.as_str())
            {
                return Err(StoreError::CredentialPairInvalid);
            }
            let occupied: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM credential_pairs WHERE account_id=1 AND slot=?1)",
                    [pair.slot.as_str()],
                    |row| row.get(0),
                )
                .map_err(StoreError::CredentialPairStore)?;
            if occupied {
                return Err(StoreError::CredentialPairInvalid);
            }
            let metadata = credential_pre_upgrade_row_metadata_on(&transaction, pair.slot)?
                .ok_or(StoreError::CredentialPairInvalid)?;
            // R46 / M2P-73(a): Fleet is always Hub-owned. Legacy ownership
            // requires both the matching output and a completed success.
            let owner = match pair.slot {
                CredentialSlot::FleetApi => RefreshOwner::Hub,
                CredentialSlot::OwnerApi => {
                    if owner_bindings.iter().any(|binding| {
                        binding.output_generation == Some(pair.generation)
                            && matches!(
                                &binding.receipt_state,
                                PreUpgradeReceiptState::Completed(OutboundRequestOutcome::Success)
                            )
                    }) {
                        RefreshOwner::Hub
                    } else {
                        RefreshOwner::External
                    }
                }
            };
            if metadata.expires_at != pair.expires_at
                || metadata.next_refresh_at != pair.next_refresh_at
                || metadata
                    .generation
                    .is_some_and(|generation| generation != pair.generation)
                || metadata.client_id != pair.client_id
                || metadata.region != pair.region
                || pair.refresh_owner != owner
            {
                return Err(StoreError::CredentialPairInvalid);
            }
            target.set_slot(pair.slot, CredentialSlotFormatState::Migrated, None);
        }
        let mut blocked_slots = std::collections::BTreeSet::new();
        for (slot, reason) in plan.blocked_slots {
            if state.slot(*slot).0 != CredentialSlotFormatState::Pending
                || converted_slots.contains(slot.as_str())
                || !blocked_slots.insert(slot.as_str())
            {
                return Err(StoreError::CredentialPairInvalid);
            }
            target.set_slot(*slot, CredentialSlotFormatState::Pending, Some(*reason));
        }
        for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
            if state.slot(slot).0 == CredentialSlotFormatState::Pending
                && !converted_slots.contains(slot.as_str())
                && !blocked_slots.contains(slot.as_str())
            {
                return Err(StoreError::CredentialPairInvalid);
            }
        }
        let persisted = credential_migration_persisted_legacy(&transaction)?;
        let mut derived = std::collections::BTreeMap::new();
        let mut counts = state.counts();
        if initial {
            let mut expected_attempts = std::collections::BTreeMap::new();
            let mut owner_ids = std::collections::BTreeSet::new();
            let mut all_ids = std::collections::BTreeSet::new();
            let mut earliest = std::collections::BTreeMap::<Uuid, i64>::new();
            for (slot, bindings) in [
                (CredentialSlot::OwnerApi, &owner_bindings),
                (CredentialSlot::FleetApi, &fleet_bindings),
            ] {
                for binding in bindings {
                    if expected_attempts
                        .insert((binding.receipt_id.0, slot.as_str()), binding)
                        .is_some()
                    {
                        return Err(StoreError::CredentialPairInvalid);
                    }
                    if slot == CredentialSlot::OwnerApi {
                        owner_ids.insert(binding.attempt_id);
                    }
                    all_ids.insert(binding.attempt_id);
                    earliest
                        .entry(binding.input_generation)
                        .and_modify(|time| *time = (*time).min(binding.started_at_ms))
                        .or_insert(binding.started_at_ms);
                }
            }
            if plan.attempts.len() != expected_attempts.len() {
                return Err(StoreError::CredentialPairInvalid);
            }
            let mut attempt_keys = std::collections::BTreeSet::new();
            let mut attempt_ids = std::collections::BTreeSet::new();
            for attempt in plan.attempts {
                let key = (attempt.receipt_id.0, attempt.slot.as_str());
                let original = expected_attempts
                    .get(&key)
                    .ok_or(StoreError::CredentialPairInvalid)?;
                let reassigned = attempt.slot == CredentialSlot::FleetApi
                    && owner_ids.contains(&original.attempt_id);
                if !attempt_keys.insert(key)
                    || !attempt_ids.insert(attempt.attempt_id)
                    || attempt.input_generation != original.input_generation
                    || attempt.output_generation != original.output_generation
                    || attempt.output_generation == Some(attempt.input_generation)
                    || (reassigned
                        && (attempt.attempt_id.get_version_num() != 4
                            || (attempt.attempt_id.as_bytes()[8] & 0xc0) != 0x80
                            || all_ids.contains(&attempt.attempt_id)))
                    || (!reassigned && attempt.attempt_id != original.attempt_id)
                {
                    return Err(StoreError::CredentialPairInvalid);
                }
            }
            let mut legacy = std::collections::BTreeMap::new();
            for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
                for generation in credential_pre_upgrade_fences_on(&transaction, slot)? {
                    legacy.insert(
                        generation,
                        earliest.get(&generation).copied().unwrap_or(plan.now_ms),
                    );
                }
            }
            if plan.legacy_fences.len() != legacy.len() {
                return Err(StoreError::CredentialPairInvalid);
            }
            let mut supplied_generations = std::collections::BTreeSet::new();
            for fence in plan.legacy_fences {
                let anchored = plan
                    .pairs
                    .iter()
                    .any(|pair| pair.generation == fence.generation);
                if !supplied_generations.insert(fence.generation)
                    || legacy.get(&fence.generation) != Some(&fence.fenced_at_ms)
                    || (fence.identity == LegacyFenceIdentity::Anchored) != anchored
                {
                    return Err(StoreError::CredentialPairInvalid);
                }
            }
            for (generation, (identity, time)) in &persisted {
                let anchored = plan.pairs.iter().any(|pair| pair.generation == *generation);
                if legacy.get(generation) != Some(time)
                    || (*identity == LegacyFenceIdentity::Anchored && !anchored)
                {
                    return Err(StoreError::CredentialPairInvalid);
                }
            }
            for pair in plan.pairs {
                if legacy.contains_key(&pair.generation) {
                    derived.insert(
                        pair.refresh_fingerprint
                            .ok_or(StoreError::CredentialPairInvalid)?,
                        plan.now_ms,
                    );
                }
            }
            credential_migration_check_refresh_vector(plan.refresh_fences, &derived, false)?;
            credential_migration_check_prospective_capacity(
                &transaction,
                &persisted,
                Some(&legacy),
                &derived,
                capacity,
            )?;
            for attempt in plan.attempts {
                transaction.execute(
                    "INSERT INTO credential_refresh_attempts(receipt_id,attempt_id,account_id,slot,
                     input_generation,fence_id,output_generation,migrated)
                     VALUES(?1,?2,1,?3,?4,NULL,?5,1)",
                    params![attempt.receipt_id.0,attempt.attempt_id.to_string(),attempt.slot.as_str(),
                        attempt.input_generation.to_string(),attempt.output_generation.map(|value| value.to_string())],
                ).map_err(StoreError::CredentialPairStore)?;
            }
            for fence in plan.legacy_fences {
                let identity = match fence.identity {
                    LegacyFenceIdentity::Anchored => "anchored",
                    LegacyFenceIdentity::Opaque => "opaque",
                };
                transaction.execute(
                    "INSERT INTO credential_legacy_fences(legacy_generation,identity,fenced_at_ms)
                     VALUES(?1,?2,?3) ON CONFLICT(legacy_generation) DO UPDATE SET
                     identity=CASE WHEN excluded.identity='anchored' THEN 'anchored' ELSE identity END",
                    params![fence.generation.to_string(),identity,fence.fenced_at_ms],
                ).map_err(StoreError::CredentialPairStore)?;
            }
            credential_migration_insert_refresh_fences(&transaction, &derived)?;
            counts = CredentialMigrationCounts {
                attempts_migrated: expected_attempts.len() as i64,
                legacy_fences: legacy.len() as i64,
                fences_fingerprinted: transaction
                    .query_row(
                        "SELECT COUNT(*) FROM credential_refresh_fences WHERE kind='migrated'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(StoreError::CredentialPairStore)?,
            };
            target.fences_pending = false;
        } else {
            credential_migration_require_mapped_history(&transaction)?;
            for pair in plan.pairs {
                if persisted.contains_key(&pair.generation) {
                    derived.insert(
                        pair.refresh_fingerprint
                            .ok_or(StoreError::CredentialPairInvalid)?,
                        plan.now_ms,
                    );
                }
            }
            credential_migration_check_refresh_vector(plan.refresh_fences, &derived, true)?;
            let additional_refresh = credential_migration_check_prospective_capacity(
                &transaction,
                &persisted,
                None,
                &derived,
                capacity,
            )?;
            let fingerprint_total = counts
                .fences_fingerprinted
                .checked_add(additional_refresh)
                .filter(|total| *total <= MAX_CREDENTIAL_REFRESH_FENCES as i64)
                .ok_or(StoreError::CredentialPairInvalid)?;
            let inserted = credential_migration_insert_refresh_fences(&transaction, &derived)?;
            if inserted != additional_refresh {
                return Err(StoreError::CredentialPairInvalid);
            }
            counts.fences_fingerprinted = fingerprint_total;
            for pair in plan.pairs {
                if persisted.contains_key(&pair.generation) {
                    transaction
                        .execute(
                            "UPDATE credential_legacy_fences SET identity='anchored'
                         WHERE legacy_generation=?1 AND identity='opaque'",
                            [pair.generation.to_string()],
                        )
                        .map_err(StoreError::CredentialPairStore)?;
                }
            }
        }
        credential_migration_check_capacity(&transaction, capacity)?;
        if !initial
            && plan.pairs.is_empty()
            && target == CredentialMigrationTarget::from_state(&state)
            && state.started_at_ms.is_some()
            && (target.owner == CredentialSlotFormatState::Pending
                || target.fleet == CredentialSlotFormatState::Pending)
        {
            // A new admitted open can replay an already-mapped blocked plan
            // without another COMMIT. Its passed barrier is still required
            // before R28/R35 may discard that blocked predecessor slot.
            drop(transaction);
            self.migration_state.conversion_barrier_passed = false;
            self.credential_migration_barrier()?;
            self.migration_state.conversion_barrier_passed = true;
            return Ok(counts);
        }
        for pair in plan.pairs {
            credential_write_pair(&transaction, pair, plan.now_ms)?;
            let table = match pair.slot {
                CredentialSlot::OwnerApi => "legacy_owner_tokens",
                CredentialSlot::FleetApi => "fleet_tokens",
            };
            let deleted = transaction
                .execute(&format!("DELETE FROM {table} WHERE singleton_id=1"), [])
                .map_err(StoreError::CredentialPairStore)?;
            if deleted != 1 {
                return Err(StoreError::CredentialPairInvalid);
            }
        }
        credential_migration_update_state(&transaction, &state, &target, counts, plan.now_ms)?;
        before_commit();
        transaction
            .commit()
            .map_err(StoreError::CredentialPairStore)?;
        self.migration_state.conversion_barrier_passed = false;
        self.migration_state.vacuum_succeeded = false;
        self.credential_migration_barrier()?;
        self.migration_state.conversion_barrier_passed = true;
        Ok(counts)
    }
}
