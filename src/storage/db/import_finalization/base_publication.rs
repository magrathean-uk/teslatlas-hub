// SPDX-License-Identifier: AGPL-3.0-only

// An import base's catalogue publication, as one step of a catalogue
// transaction: a plain first base, a replacement of a full import lineage,
// or the window rebind (DEC-150) whose seam follows in the same transaction
// (DEC-151 follow-up, M03-T24).

/// What an import base publication writes.
struct ImportBasePublication<'a> {
    run_id: Uuid,
    source_id: Uuid,
    vehicle_id: Uuid,
    car_id: i64,
    updated_at_ms: i64,
    manifest: &'a SyncManifest,
    geofences: &'a [crate::source_db_projection::SourceDbGeofence],
    binding: &'a ProjectionBinding,
    materialisation: Option<(&'a ProjectionCar, &'a [ProjectionDrive])>,
    retain_legacy_inventory: bool,
    replacement: Option<BaseReplacement<'a>>,
}

impl ImportBasePublication<'_> {
    /// The checks made before the transaction begins.
    fn validate(&self) -> Result<(), StoreError> {
        if self.run_id.is_nil()
            || self.source_id.is_nil()
            || self.vehicle_id.is_nil()
            || self.car_id <= 0
            || self.manifest.vehicle_id != self.vehicle_id
        {
            return Err(StoreError::InvalidImportGeneration);
        }
        validate_immutable_v2_base_binding(self.manifest, self.binding)?;
        // The staging generation feeds the imported lifecycle materialisation,
        // while the V2 binding owns the pack/state scope.  They must describe
        // the exact same source/car; otherwise a valid selected-car base could
        // be published next to lifecycle rows attributed to another car.
        if self.source_id != self.binding.account_id || self.car_id != self.binding.selected_car_id {
            return Err(StoreError::LineageCatalogConflict);
        }
        Ok(())
    }

    fn rebinds_window(&self) -> bool {
        matches!(self.replacement, Some(BaseReplacement::Window(_)))
    }
}

/// Publishes an import base in `transaction`: the catalogue, the imported
/// materialisation, the retired lineage and the projection state attached
/// as `transfer` (whose head is the base's). A window rebind also keeps the
/// Hub's rows and journals them again; its seam and the generation's end
/// follow in the same transaction. Commits nothing.
fn publish_import_base_in_transaction(
    transaction: &Transaction<'_>,
    publication: &ImportBasePublication<'_>,
    transfer: &SourceDbProjectionStateTransfer,
) -> Result<(), StoreError> {
    let ImportBasePublication {
        run_id,
        source_id,
        vehicle_id,
        car_id,
        manifest,
        geofences,
        binding,
        materialisation,
        retain_legacy_inventory,
        replacement,
        ..
    } = *publication;
    let mut rebind_journal_from = None;
    if publication.rebinds_window() {
        // The rebind keeps the Hub's collection for the seam the same
        // transaction publishes on it (DEC-150).
        if !window_only_v2_base_in_connection(transaction, &vehicle_id.to_string())? {
            return Err(StoreError::LineageCatalogConflict);
        }
        rebind_journal_from = Some(rebind_window_rows_in_transaction(transaction, vehicle_id, car_id)?);
    } else {
        // A publication that does not plan the seam may not publish
        // over the Hub's own collection (design M03-SEAM-R3, section 4).
        refuse_unplanned_import_seam_in_transaction(transaction, &vehicle_id.to_string())?;
    }
    staging_import_generation_session_in_transaction(transaction, run_id, source_id, vehicle_id, car_id)?;
    if let Some((car, drives)) = materialisation {
        reconcile_imported_materialisation_in_transaction(
            transaction,
            vehicle_id,
            car_id,
            car,
            drives,
            ImportedProjectionRows::Attached,
            if let Some(BaseReplacement::Import(_)) = replacement {
                PriorImportedDriveRows::Durable
            } else {
                PriorImportedDriveRows::None
            },
        )?;
        if let Some(from_revision) = rebind_journal_from {
            journal_rebound_rows_in_transaction(transaction, vehicle_id, car_id, drives, from_revision)?;
        }
    }
    match replacement {
        Some(BaseReplacement::Import(expected)) => {
            retire_import_lineage_in_transaction(transaction, expected, binding)?;
        }
        Some(BaseReplacement::Window(expected)) => {
            retire_lineage_in_transaction(transaction, expected, None)?;
        }
        None => {}
    }
    publish_manifest_in_transaction(transaction, manifest, Some(binding))?;
    replace_source_db_import_projection_state_from_attached_in_transaction(
        transaction,
        vehicle_id,
        binding.account_id,
        manifest.snapshot_id,
        manifest.head_sequence,
        binding.selected_car_id,
        transfer,
        true,
    )?;
    if retain_legacy_inventory {
        replace_source_db_import_projection_inventory_from_attached_in_transaction(
            transaction,
            vehicle_id,
            binding.account_id,
            manifest.snapshot_id,
            manifest.head_sequence,
            binding.selected_car_id,
            transfer,
            true,
        )?;
    }
    upsert_geofences_in_transaction(transaction, vehicle_id, geofences)?;
    Ok(())
}

/// Ends a base publication's generation in `transaction`: its open session
/// is promoted against the state it began from, the source fingerprint is
/// recorded and the generation is consumed. A window rebind's seam ends it
/// instead.
fn finish_import_base_generation_in_transaction(
    transaction: &Transaction<'_>,
    publication: &ImportBasePublication<'_>,
    fingerprint: Sha256Digest,
) -> Result<(), StoreError> {
    if publication.rebinds_window() {
        return Err(StoreError::ImportGenerationConflict);
    }
    let (session, base_last_observation_id, base_updated_at_ms) = staging_import_generation_session_in_transaction(
        transaction,
        publication.run_id,
        publication.source_id,
        publication.vehicle_id,
        publication.car_id,
    )?;
    promote_imported_open_session_in_transaction(
        transaction,
        publication.source_id,
        publication.vehicle_id,
        publication.car_id,
        &session,
        publication.updated_at_ms,
        Some((base_last_observation_id, base_updated_at_ms)),
    )?;
    record_snapshot_fingerprint_in_transaction(transaction, publication.manifest, fingerprint)?;
    consume_import_generation_in_transaction(transaction, publication.run_id)
}

/// The open session a staging generation holds, with the lifecycle state
/// it began from.
fn staging_import_generation_session_in_transaction(
    transaction: &Transaction<'_>,
    run_id: Uuid,
    source_id: Uuid,
    vehicle_id: Uuid,
    car_id: i64,
) -> Result<(SourceDbOpenSession, i64, i64), StoreError> {
    let (encoded, base_last_observation_id, base_updated_at_ms): (String, i64, i64) = transaction
        .query_row(
            "SELECT sessions.session_json, generations.base_last_observation_id,
                    generations.base_updated_at_ms
             FROM import_generation_sessions AS sessions
             JOIN import_generations AS generations USING(run_id)
             WHERE generations.run_id = ?1 AND generations.source_id = ?2
               AND generations.vehicle_id = ?3 AND generations.car_id = ?4
               AND generations.status = 'staging'",
            params![run_id.to_string(), source_id.to_string(), vehicle_id.to_string(), car_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::ImportGeneration)?
        .ok_or(StoreError::ImportGenerationNotFound)?;
    let session = serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
    Ok((session, base_last_observation_id, base_updated_at_ms))
}

/// Consumes a staging generation once its publication is in the transaction.
fn consume_import_generation_in_transaction(
    transaction: &Connection,
    run_id: Uuid,
) -> Result<(), StoreError> {
    consume_import_generation_with_history_in(transaction, run_id, true)
}

/// As [`consume_import_generation_in_transaction`] for a promotion that
/// publishes nothing: the imported history did not change.
fn consume_unchanged_import_generation_in_transaction(
    transaction: &Connection,
    run_id: Uuid,
) -> Result<(), StoreError> {
    consume_import_generation_with_history_in(transaction, run_id, false)
}

fn consume_import_generation_with_history_in(
    transaction: &Connection,
    run_id: Uuid,
    changes_history: bool,
) -> Result<(), StoreError> {
    if transaction
        .execute(
            "DELETE FROM import_generations WHERE run_id = ?1 AND status = 'staging'",
            params![run_id.to_string()],
        )
        .map_err(StoreError::ImportGeneration)?
        != 1
    {
        return Err(StoreError::ImportGenerationNotFound);
    }
    // Issue #60: the same commit makes a captured import, or one that
    // changes history without a capture, the vehicle's import revision, so
    // no earlier physical head stays served beside it.
    commit_import_publication_revision_in(transaction, run_id, changes_history)
}
