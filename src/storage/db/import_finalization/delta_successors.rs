// SPDX-License-Identifier: AGPL-3.0-only

// A batch of import delta successors' catalogue publication, as one step of
// a catalogue transaction: on its own, or as the seam a window rebind
// publishes on its new base in the same transaction (DEC-151 follow-up,
// M03-T24).

/// What a delta successor publication writes.
struct DeltaSuccessorPublication<'a> {
    run_id: Uuid,
    source_id: Uuid,
    vehicle_id: Uuid,
    car_id: i64,
    updated_at_ms: i64,
    deltas: &'a [LineageDelta],
    terminal_cursor: &'a OpaqueCursor,
    fingerprint: Sha256Digest,
    geofences: &'a [crate::source_db_projection::SourceDbGeofence],
    materialisation: Option<(&'a ProjectionCar, &'a [ProjectionDrive])>,
    retain_legacy_inventory: bool,
    seam: Option<ImportSeamPublication<'a>>,
}

/// How a delta successor publication writes the import's projection state.
#[derive(Clone, Copy)]
enum SuccessorProjectionState<'a> {
    /// Replaces the durable state with the attached transfer.
    Replace(&'a SourceDbProjectionStateTransfer),
    /// The same transaction already wrote the final state on the base: only
    /// its head advances to the batch's.
    AdvanceHead,
}

impl DeltaSuccessorPublication<'_> {
    fn final_delta(&self) -> Result<&LineageDelta, StoreError> {
        if self.run_id.is_nil() || self.source_id.is_nil() || self.vehicle_id.is_nil() || self.car_id <= 0 {
            return Err(StoreError::InvalidImportGeneration);
        }
        self.deltas.last().ok_or(StoreError::LineageCatalogConflict)
    }

    /// Validates the exact post-commit lineage before any catalogue row is
    /// written. This enforces the aggregate row/byte/pack ceilings across the
    /// existing immutable base plus this whole bounded batch, rather than
    /// merely checking every delta in isolation.
    fn validate_lineage(&self, mut candidate: LineageManifestV2) -> Result<(), StoreError> {
        let final_delta = self.final_delta()?;
        candidate.deltas.extend_from_slice(self.deltas);
        candidate.head_sequence = final_delta.to_sequence;
        candidate.head_digest = final_delta.chain_digest;
        candidate.terminal_cursor = self.terminal_cursor.clone();
        candidate
            .validate_with_limits(ProtocolLimits::default())
            .map_err(StoreError::Manifest)
    }
}

impl HubStore {
    /// The checks of a delta successor batch made before its transaction:
    /// every pack against `binding`, their order and the terminal cursor.
    fn verify_delta_successors(
        &self,
        successors: &DeltaSuccessorPublication<'_>,
        binding: &ProjectionBinding,
        cursor_key: &CursorKey,
    ) -> Result<(), StoreError> {
        let final_delta = successors.final_delta()?;
        let first_delta = &successors.deltas[0];
        let mut prior_to = None;
        let mut prior_chain = None;
        let mut prior_ordinal = None;
        for delta in successors.deltas {
            validate_import_delta_successor_shape(delta)?;
            self.verify_import_delta_pack(delta, binding)?;
            if delta.pack.snapshot_id != first_delta.pack.snapshot_id
                || prior_to.is_some_and(|value| delta.from_sequence != value)
                || prior_chain.is_some_and(|value| delta.parent_chain_digest != value)
                || prior_ordinal.is_some_and(|value| delta.pack.ordinal <= value)
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            prior_to = Some(delta.to_sequence);
            prior_chain = Some(delta.chain_digest);
            prior_ordinal = Some(delta.pack.ordinal);
        }
        let cursor_claims = successors
            .terminal_cursor
            .verify(cursor_key)
            .map_err(StoreError::Manifest)?;
        if cursor_claims.protocol != crate::protocol::PROTOCOL_V1
            || cursor_claims.schema != HUB_PROJECTION_SCHEMA_V2
            || cursor_claims.installation_id != binding.installation_id
            || cursor_claims.account_id != binding.account_id
            || cursor_claims.vehicle_id != binding.vehicle_id
            || cursor_claims.generation != binding.generation
            || cursor_claims.sequence != final_delta.to_sequence
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        Ok(())
    }
}

/// Publishes a verified delta successor batch in `transaction` on the
/// vehicle's current head: the seam it carries, its catalogue rows, the
/// imported materialisation, the projection state, the promoted open
/// session, the fingerprint, and the generation's end. Commits nothing.
fn publish_delta_successors_in_transaction(
    transaction: &Transaction<'_>,
    successors: &DeltaSuccessorPublication<'_>,
    binding: &ProjectionBinding,
    projection_state: SuccessorProjectionState<'_>,
) -> Result<(), StoreError> {
    let DeltaSuccessorPublication {
        run_id,
        source_id,
        vehicle_id,
        car_id,
        updated_at_ms,
        deltas,
        terminal_cursor,
        fingerprint,
        geofences,
        materialisation,
        retain_legacy_inventory,
        seam,
    } = *successors;
    let final_delta = successors.final_delta()?;
    let first_delta = &deltas[0];
    let terminal_cursor_json =
        serde_json::to_string(terminal_cursor).map_err(StoreError::SerializeManifest)?;
    let vehicle_key = vehicle_id.to_string();
    let (session, base_last_observation_id, base_updated_at_ms) =
        staging_import_generation_session_in_transaction(transaction, run_id, source_id, vehicle_id, car_id)?;
    let base_snapshot: String = transaction
        .query_row(
            "SELECT snapshot_id FROM sync_bases WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?
        .ok_or(StoreError::LineageCatalogConflict)?;
    if base_snapshot != first_delta.pack.snapshot_id.to_string() {
        return Err(StoreError::LineageCatalogConflict);
    }
    let (initial_head_sequence, initial_head_digest): (i64, String) = transaction
        .query_row(
            "SELECT head_sequence, head_digest FROM sync_heads WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?
        .ok_or(StoreError::LineageCatalogConflict)?;
    if initial_head_sequence
        != i64::try_from(first_delta.from_sequence)
            .map_err(|_| StoreError::SequenceTooLarge)?
        || initial_head_digest != first_delta.parent_chain_digest.to_string()
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    // Delta ordinals identify the physical pack order inside the immutable
    // base snapshot.  A strictly increasing ordinal alone permits a
    // caller to skip an unused ordinal; require the exact next catalogue
    // slot instead, while the IMMEDIATE transaction holds the write lock.
    let maximum_ordinal: Option<i64> = transaction
        .query_row(
            "SELECT MAX(ordinal) FROM sync_packs WHERE snapshot_id = ?1",
            params![base_snapshot.as_str()],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    let mut expected_ordinal = maximum_ordinal
        .unwrap_or(-1)
        .checked_add(1)
        .and_then(|ordinal| u32::try_from(ordinal).ok())
        .ok_or(StoreError::LineageCatalogConflict)?;
    if let (Some(seam), Some((_, drives))) = (seam, materialisation) {
        apply_staged_import_seam_in_transaction(transaction, vehicle_id, drives, seam, deltas, binding)?;
    }
    let mut expected_sequence = first_delta.from_sequence;
    let mut expected_digest = first_delta.parent_chain_digest;
    for delta in deltas {
        if delta.pack.snapshot_id.to_string() != base_snapshot
            || delta.from_sequence != expected_sequence
            || delta.parent_chain_digest != expected_digest
            || delta.pack.ordinal != expected_ordinal
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        insert_import_delta_in_transaction(transaction, &vehicle_key, delta)?;
        expected_sequence = delta.to_sequence;
        expected_digest = delta.chain_digest;
        expected_ordinal = expected_ordinal
            .checked_add(1)
            .ok_or(StoreError::LineageCatalogConflict)?;
    }
    let updated = transaction
        .execute(
            "UPDATE sync_heads SET head_sequence = ?1, head_digest = ?2,
                terminal_cursor = ?3
         WHERE vehicle_id = ?4 AND head_sequence = ?5
           AND head_digest = ?6",
            params![
                i64::try_from(final_delta.to_sequence)
                    .map_err(|_| StoreError::SequenceTooLarge)?,
                final_delta.chain_digest.to_string(),
                terminal_cursor_json,
                vehicle_key.as_str(),
                initial_head_sequence,
                initial_head_digest,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    if updated != 1 {
        return Err(StoreError::LineageCatalogConflict);
    }
    publish_staged_import_seam_in_transaction(transaction, vehicle_id, seam, deltas)?;
    match materialisation {
        Some((car, drives)) => {
            // The packs were written from a plan; publication plans
            // again from the same store and refuses another one.
            let charge_seam =
                replan_import_seam_in_transaction(transaction, vehicle_id, car_id, drives, seam)?;
            reconcile_imported_materialisation_in_transaction(
                transaction,
                vehicle_id,
                car_id,
                car,
                drives,
                ImportedProjectionRows::Attached,
                PriorImportedDriveRows::Durable,
            )?;
            if let Some((plan, charges)) = charge_seam {
                apply_import_seam_to_charges_in_transaction(
                    transaction, vehicle_id, &plan, charges, true,
                )?;
            }
        }
        // A successor that does not plan the seam may not publish
        // over the Hub's own collection.
        None => refuse_unplanned_import_seam_in_transaction(transaction, &vehicle_key)?,
    }
    match projection_state {
        SuccessorProjectionState::Replace(transfer) => {
            replace_source_db_import_projection_state_from_attached_in_transaction(
                transaction,
                vehicle_id,
                binding.account_id,
                final_delta.pack.snapshot_id,
                final_delta.to_sequence,
                binding.selected_car_id,
                transfer,
                false,
            )?;
            if retain_legacy_inventory {
                replace_source_db_import_projection_inventory_from_attached_in_transaction(
                    transaction,
                    vehicle_id,
                    binding.account_id,
                    final_delta.pack.snapshot_id,
                    final_delta.to_sequence,
                    binding.selected_car_id,
                    transfer,
                    false,
                )?;
            }
        }
        SuccessorProjectionState::AdvanceHead => {
            if retain_legacy_inventory {
                return Err(StoreError::LineageCatalogConflict);
            }
            advance_source_db_import_projection_state_head_in_transaction(
                transaction,
                vehicle_id,
                final_delta.pack.snapshot_id,
                final_delta.to_sequence,
            )?;
        }
    }
    promote_imported_open_session_in_transaction(
        transaction,
        source_id,
        vehicle_id,
        car_id,
        &session,
        updated_at_ms,
        Some((base_last_observation_id, base_updated_at_ms)),
    )?;
    upsert_geofences_in_transaction(transaction, vehicle_id, geofences)?;
    transaction
        .execute(
            "INSERT INTO snapshot_fingerprints(
            vehicle_id, fingerprint_sha256, snapshot_id, head_sequence
         ) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(vehicle_id) DO UPDATE SET
            fingerprint_sha256 = excluded.fingerprint_sha256,
            snapshot_id = excluded.snapshot_id,
            head_sequence = excluded.head_sequence",
            params![
                vehicle_key.as_str(),
                fingerprint.as_bytes().as_slice(),
                final_delta.pack.snapshot_id.to_string(),
                i64::try_from(final_delta.to_sequence)
                    .map_err(|_| StoreError::SequenceTooLarge)?,
            ],
        )
        .map_err(StoreError::PublishManifest)?;
    consume_import_generation_in_transaction(transaction, run_id)
}

/// Advances the durable projection state's head to `head_sequence` on the
/// base `base_snapshot_id`, whose rows the same transaction already wrote.
fn advance_source_db_import_projection_state_head_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    base_snapshot_id: Uuid,
    head_sequence: u64,
) -> Result<(), StoreError> {
    let updated = transaction
        .execute(
            concat!(
                "UPDATE ",
                crate::db::import_table_names::import_projection_state_heads_table!(),
                " SET head_sequence = ?1 WHERE vehicle_id = ?2 AND base_snapshot_id = ?3
                   AND head_sequence < ?1"
            ),
            params![
                i64::try_from(head_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                vehicle_id.to_string(),
                base_snapshot_id.to_string(),
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    if updated != 1 {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}
