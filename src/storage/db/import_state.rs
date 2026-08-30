// SPDX-License-Identifier: AGPL-3.0-only

fn validate_immutable_v2_base_binding(
    manifest: &SyncManifest,
    binding: &ProjectionBinding,
) -> Result<(), StoreError> {
    if manifest.schema != HUB_PROJECTION_SCHEMA_V2
        || manifest.mode != crate::protocol::TransferMode::FullSnapshot
        || manifest.installation_id != binding.installation_id
        || manifest.account_id != binding.account_id
        || manifest.vehicle_id != binding.vehicle_id
        || manifest.generation != binding.generation
        || binding.selected_car_id <= 0
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

fn legacy_v2_base_description(
    payload: &[u8],
) -> Result<Option<LegacyV2BaseDescription>, StoreError> {
    match serde_json::from_slice::<SyncManifest>(payload) {
        Ok(manifest) => {
            validate_manifest_for_catalogue(&manifest)?;
            if manifest.schema != HUB_PROJECTION_SCHEMA_V2 {
                return Ok(None);
            }
            if manifest.mode != crate::protocol::TransferMode::FullSnapshot
                || manifest.base_sequence != manifest.head_sequence
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            let base_digest = manifest
                .chunks
                .first()
                .map(|pack| pack.sha256)
                .ok_or(StoreError::LineageCatalogConflict)?;
            Ok(Some(LegacyV2BaseDescription {
                installation_id: manifest.installation_id,
                account_id: manifest.account_id,
                vehicle_id: manifest.vehicle_id,
                generation: manifest.generation,
                snapshot_id: manifest.snapshot_id,
                base_sequence: manifest.head_sequence,
                base_digest,
                packs: manifest.chunks,
            }))
        }
        Err(sync_error) => {
            let lineage = match serde_json::from_slice::<LineageManifestV2>(payload) {
                Ok(lineage) => lineage,
                Err(_) => return Err(StoreError::DeserializeManifest(sync_error)),
            };
            lineage.validate().map_err(StoreError::Manifest)?;
            if lineage.schema != HUB_PROJECTION_SCHEMA_V2 {
                return Ok(None);
            }
            if lineage.base.packs.first().map(|pack| pack.sha256) != Some(lineage.base.digest) {
                return Err(StoreError::LineageCatalogConflict);
            }
            Ok(Some(LegacyV2BaseDescription {
                installation_id: lineage.installation_id,
                account_id: lineage.account_id,
                vehicle_id: lineage.vehicle_id,
                generation: lineage.generation,
                snapshot_id: lineage.base.snapshot_id,
                base_sequence: lineage.base.sequence,
                base_digest: lineage.base.digest,
                packs: lineage.base.packs,
            }))
        }
    }
}

fn record_immutable_v2_base_binding_in_transaction(
    transaction: &Transaction<'_>,
    manifest: &SyncManifest,
    binding: &ProjectionBinding,
) -> Result<(), StoreError> {
    validate_immutable_v2_base_binding(manifest, binding)?;
    record_immutable_v2_base_binding_values_in_transaction(
        transaction,
        manifest.vehicle_id,
        manifest.snapshot_id,
        binding,
    )
}

fn record_immutable_v2_base_binding_values_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    snapshot_id: Uuid,
    binding: &ProjectionBinding,
) -> Result<(), StoreError> {
    if vehicle_id.is_nil()
        || snapshot_id.is_nil()
        || binding.vehicle_id != vehicle_id
        || binding.installation_id.is_nil()
        || binding.account_id.is_nil()
        || binding.generation == 0
        || binding.selected_car_id <= 0
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    let base_exists: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_bases
                 WHERE vehicle_id = ?1 AND snapshot_id = ?2
            )",
            params![vehicle_id.to_string(), snapshot_id.to_string()],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !base_exists {
        return Err(StoreError::LineageCatalogConflict);
    }
    let existing: Option<(String, String, i64, i64)> = transaction
        .query_row(
            "SELECT installation_id, account_id, generation, selected_car_id
               FROM v2_base_bindings WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    let expected_generation =
        i64::try_from(binding.generation).map_err(|_| StoreError::SequenceTooLarge)?;
    if let Some((installation_id, account_id, generation, selected_car_id)) = existing {
        if installation_id != binding.installation_id.to_string()
            || account_id != binding.account_id.to_string()
            || generation != expected_generation
            || selected_car_id != binding.selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        return Ok(());
    }
    transaction
        .execute(
            "INSERT INTO v2_base_bindings(
                vehicle_id, snapshot_id, installation_id, account_id,
                generation, selected_car_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                vehicle_id.to_string(),
                snapshot_id.to_string(),
                binding.installation_id.to_string(),
                binding.account_id.to_string(),
                expected_generation,
                binding.selected_car_id,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    Ok(())
}

fn teslamate_inventory_entity_name(entity: ProjectionDeltaEntity) -> Option<&'static str> {
    match entity {
        ProjectionDeltaEntity::Drive => Some("drive"),
        ProjectionDeltaEntity::Position => Some("position"),
        ProjectionDeltaEntity::Charge => Some("charge"),
        ProjectionDeltaEntity::ChargeSample => Some("charge_sample"),
        ProjectionDeltaEntity::State => Some("state"),
        ProjectionDeltaEntity::Update => Some("update"),
        ProjectionDeltaEntity::Car
        | ProjectionDeltaEntity::CarSetting
        | ProjectionDeltaEntity::Geofence
        | ProjectionDeltaEntity::Address => None,
    }
}

fn teslamate_inventory_entity(value: &str) -> Option<ProjectionDeltaEntity> {
    match value {
        "drive" => Some(ProjectionDeltaEntity::Drive),
        "position" => Some(ProjectionDeltaEntity::Position),
        "charge" => Some(ProjectionDeltaEntity::Charge),
        "charge_sample" => Some(ProjectionDeltaEntity::ChargeSample),
        "state" => Some(ProjectionDeltaEntity::State),
        "update" => Some(ProjectionDeltaEntity::Update),
        _ => None,
    }
}

fn stored_projection_state_entity(
    name: &str,
    ordinal: i64,
) -> Result<TeslaMateProjectionStateEntity, StoreError> {
    let by_name: TeslaMateProjectionStateEntity =
        name.parse().map_err(StoreError::TeslaMateProjectionState)?;
    let by_ordinal = TeslaMateProjectionStateEntity::from_ordinal(ordinal)
        .map_err(StoreError::TeslaMateProjectionState)?;
    if by_name != by_ordinal {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(by_name)
}

fn projection_state_digest_from_blob(blob: Vec<u8>) -> Result<Sha256Digest, StoreError> {
    let digest: [u8; 32] = blob
        .try_into()
        .map_err(|_: Vec<u8>| TeslaMateProjectionStateError::InvalidStoredDigest)?;
    Ok(Sha256Digest::from_bytes(digest))
}

fn validate_teslamate_import_inventory_rows(
    selected_car_id: i64,
    rows: &[ProjectionTombstone],
) -> Result<(), StoreError> {
    if selected_car_id <= 0 {
        return Err(StoreError::LineageCatalogConflict);
    }
    let mut seen = HashSet::with_capacity(rows.len());
    for row in rows {
        if row.id <= 0
            || row.car_id != selected_car_id
            || teslamate_inventory_entity_name(row.entity).is_none()
            || !seen.insert((row.entity, row.id))
        {
            return Err(StoreError::LineageCatalogConflict);
        }
    }
    Ok(())
}

fn validate_teslamate_import_inventory(
    manifest: &SyncManifest,
    binding: &ProjectionBinding,
    inventory: &TeslaMateImportProjectionInventory,
) -> Result<(), StoreError> {
    validate_immutable_v2_base_binding(manifest, binding)?;
    if inventory.source_id.is_nil()
        || inventory.source_id != binding.account_id
        || inventory.selected_car_id != binding.selected_car_id
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    validate_teslamate_import_inventory_rows(inventory.selected_car_id, &inventory.rows)
}

fn validate_teslamate_import_delta_inventory(
    delta: &LineageDelta,
    binding: &ProjectionBinding,
    inventory: &TeslaMateImportProjectionInventory,
) -> Result<(), StoreError> {
    if inventory.source_id.is_nil()
        || inventory.source_id != binding.account_id
        || inventory.selected_car_id != binding.selected_car_id
        || delta.pack.snapshot_id.is_nil()
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    validate_teslamate_import_inventory_rows(inventory.selected_car_id, &inventory.rows)
}

fn replace_teslamate_import_inventory_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    base_snapshot_id: Uuid,
    head_sequence: u64,
    inventory: &TeslaMateImportProjectionInventory,
    allow_create: bool,
) -> Result<(), StoreError> {
    if vehicle_id.is_nil() || base_snapshot_id.is_nil() {
        return Err(StoreError::LineageCatalogConflict);
    }
    validate_teslamate_import_inventory_rows(inventory.selected_car_id, &inventory.rows)?;
    let head_sequence = i64::try_from(head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let vehicle_key = vehicle_id.to_string();
    let base_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_bases AS base
                JOIN sync_heads AS head ON head.vehicle_id = base.vehicle_id
                 WHERE base.vehicle_id = ?1
                   AND base.snapshot_id = ?2
                   AND head.base_snapshot_id = base.snapshot_id
                   AND head.head_sequence = ?3
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                head_sequence
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !base_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let existing: Option<(String, String, i64)> = transaction
        .query_row(
            "SELECT source_id, base_snapshot_id, selected_car_id
               FROM teslamate_import_projection_heads WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if let Some((source_id, snapshot_id, selected_car_id)) = existing {
        if source_id != inventory.source_id.to_string()
            || snapshot_id != base_snapshot_id.to_string()
            || selected_car_id != inventory.selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
    } else if !allow_create {
        return Err(StoreError::TeslaMateImportInventoryMissing(vehicle_id));
    }
    transaction
        .execute(
            "INSERT INTO teslamate_import_projection_heads(
                vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(vehicle_id) DO UPDATE SET head_sequence = excluded.head_sequence",
            params![
                vehicle_key.as_str(),
                inventory.source_id.to_string(),
                base_snapshot_id.to_string(),
                inventory.selected_car_id,
                head_sequence,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "DELETE FROM teslamate_import_projection_rows WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO teslamate_import_projection_rows(vehicle_id, entity, entity_id)
             VALUES (?1, ?2, ?3)",
        )
        .map_err(StoreError::LineageCatalog)?;
    for row in &inventory.rows {
        statement
            .execute(params![
                vehicle_key.as_str(),
                teslamate_inventory_entity_name(row.entity)
                    .expect("validated TeslaMate inventory entity"),
                row.id,
            ])
            .map_err(StoreError::LineageCatalog)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct LegacyDirectBridgeCandidate {
    snapshot_id: Uuid,
    head_sequence: u64,
    total_rows: u64,
    legacy_fingerprint: Sha256Digest,
}

/// Identify exactly the inventory-only V2 base shape produced by the retired
/// direct importer. This deliberately proves every relationship instead of
/// treating a missing state head as evidence that an upgrade is safe.
fn legacy_direct_bridge_candidate(
    connection: &Connection,
    vehicle_id: Uuid,
    source_id: Uuid,
    selected_car_id: i64,
) -> Result<Option<LegacyDirectBridgeCandidate>, StoreError> {
    if vehicle_id.is_nil() || source_id.is_nil() || selected_car_id <= 0 {
        return Ok(None);
    }
    let row: Option<(String, i64, Vec<u8>, Vec<u8>)> = connection
        .query_row(
            "SELECT base.snapshot_id, head.head_sequence,
                    fingerprints.fingerprint_sha256, manifests.manifest_json
               FROM sync_bases AS base
               JOIN sync_heads AS head
                 ON head.vehicle_id = base.vehicle_id
                AND head.base_snapshot_id = base.snapshot_id
               JOIN v2_base_bindings AS binding
                 ON binding.vehicle_id = base.vehicle_id
                AND binding.snapshot_id = base.snapshot_id
               JOIN teslamate_import_projection_heads AS inventory
                 ON inventory.vehicle_id = base.vehicle_id
                AND inventory.base_snapshot_id = base.snapshot_id
               JOIN snapshot_fingerprints AS fingerprints
                 ON fingerprints.vehicle_id = base.vehicle_id
                AND fingerprints.snapshot_id = base.snapshot_id
                AND fingerprints.head_sequence = head.head_sequence
               JOIN sync_manifests AS manifests
                 ON manifests.vehicle_id = base.vehicle_id
                AND manifests.snapshot_id = base.snapshot_id
                AND manifests.head_sequence = head.head_sequence
              WHERE base.vehicle_id = ?1
                AND binding.account_id = ?2
                AND binding.selected_car_id = ?3
                AND inventory.source_id = ?2
                AND inventory.selected_car_id = ?3
                AND inventory.head_sequence = head.head_sequence
                AND base.base_sequence = head.head_sequence
                AND NOT EXISTS(
                    SELECT 1 FROM sync_deltas
                     WHERE vehicle_id = base.vehicle_id
                )
                AND NOT EXISTS(
                    SELECT 1 FROM teslamate_import_projection_state_heads
                     WHERE vehicle_id = base.vehicle_id
                )
                AND NOT EXISTS(
                    SELECT 1 FROM teslamate_import_projection_state_bridges
                     WHERE vehicle_id = base.vehicle_id
                )",
            params![
                vehicle_id.to_string(),
                source_id.to_string(),
                selected_car_id,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    let Some((snapshot_id, head_sequence, legacy_bytes, manifest_bytes)) = row else {
        return Ok(None);
    };
    let Ok(snapshot_id) = Uuid::parse_str(&snapshot_id) else {
        return Ok(None);
    };
    let Ok(head_sequence) = u64::try_from(head_sequence) else {
        return Ok(None);
    };
    let Ok(legacy_bytes) = <[u8; 32]>::try_from(legacy_bytes) else {
        return Ok(None);
    };
    let Ok(manifest) = decode_manifest(manifest_bytes) else {
        return Ok(None);
    };
    if manifest.vehicle_id != vehicle_id
        || manifest.snapshot_id != snapshot_id
        || manifest.head_sequence != head_sequence
        || manifest.schema != HUB_PROJECTION_SCHEMA_V2
    {
        return Ok(None);
    }
    Ok(Some(LegacyDirectBridgeCandidate {
        snapshot_id,
        head_sequence,
        total_rows: manifest.total_rows,
        legacy_fingerprint: Sha256Digest::from_bytes(legacy_bytes),
    }))
}

/// The old inventory must exactly describe every non-car row in the newly
/// captured durable state. The enclosing transaction has not committed yet,
/// so a failed comparison leaves neither replacement state nor marker behind.
fn legacy_projection_inventory_matches_state_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<bool, StoreError> {
    transaction
        .query_row(
            "SELECT
                NOT EXISTS(
                    SELECT 1
                      FROM teslamate_import_projection_rows AS inventory
                     WHERE inventory.vehicle_id = ?1
                       AND NOT EXISTS(
                           SELECT 1
                             FROM teslamate_import_projection_state_rows AS state
                            WHERE state.vehicle_id = inventory.vehicle_id
                              AND state.entity = inventory.entity
                              AND state.entity_id = inventory.entity_id
                       )
                )
                AND NOT EXISTS(
                    SELECT 1
                      FROM teslamate_import_projection_state_rows AS state
                     WHERE state.vehicle_id = ?1
                       AND state.entity <> 'car'
                       AND NOT EXISTS(
                           SELECT 1
                             FROM teslamate_import_projection_rows AS inventory
                            WHERE inventory.vehicle_id = state.vehicle_id
                              AND inventory.entity = state.entity
                              AND inventory.entity_id = state.entity_id
                       )
                )",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)
}

fn legacy_direct_bridge_state_error(vehicle_id: Uuid, error: StoreError) -> StoreError {
    match error {
        StoreError::LineageCatalogConflict
        | StoreError::ImmutableBaseBindingMissing(_)
        | StoreError::TeslaMateImportInventoryMissing(_)
        | StoreError::TeslaMateImportProjectionStateMissing(_)
        | StoreError::TeslaMateProjectionState(_) => {
            StoreError::TeslaMateLegacyDirectRebaseRequired(vehicle_id)
        }
        error => error,
    }
}

/// Attach a previously verified state spool only through SQLite's read-only
/// URI mode, then authenticate the attachment once more before a catalogue
/// transaction can read it. The fixed schema name is internal and never
/// accepts a caller-controlled identifier.
fn attach_teslamate_projection_state_transfer(
    connection: &Connection,
    transfer: &TeslaMateProjectionStateTransfer,
) -> Result<(), StoreError> {
    let uri = transfer.read_only_attachment_uri()?;
    connection
        .execute(
            "ATTACH DATABASE ?1 AS teslamate_projection_state_spool",
            params![uri],
        )
        .map_err(StoreError::LineageCatalog)?;
    if let Err(error) = transfer.validate_attached(connection) {
        // Validation happens before the Hub transaction begins. An explicit
        // detach keeps this connection reusable on the normal error path;
        // dropping the connection remains a final close-on-error backstop.
        let _ = connection.execute_batch("DETACH DATABASE teslamate_projection_state_spool");
        return Err(error.into());
    }
    Ok(())
}

fn detach_teslamate_projection_state_transfer(
    store: &HubStore,
    connection: &Connection,
) -> Result<(), StoreError> {
    #[cfg(not(test))]
    let _ = store;
    #[cfg(test)]
    {
        let mut fault = store
            .projection_state_detach_fault
            .lock()
            .expect("projection-state detach fault lock");
        if *fault {
            *fault = false;
            return Err(StoreError::InjectedProjectionStateDetachFault);
        }
    }
    connection
        .execute_batch("DETACH DATABASE teslamate_projection_state_spool")
        .map_err(StoreError::LineageCatalog)
}

/// A commit cannot be rolled back after the transfer attachment is no longer
/// needed. If SQLite then declines the best-effort detach, closing this local
/// connection releases the attachment; reporting an error would make callers
/// discard packs that the committed catalogue now owns.
fn finish_teslamate_projection_state_transfer<T>(
    result: Result<T, StoreError>,
    detach: Result<(), StoreError>,
) -> Result<T, StoreError> {
    match (result, detach) {
        (Err(error), _) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
        (Ok(value), Err(error)) => {
            tracing::warn!(%error, "could not detach committed TeslaMate projection-state spool; closing connection releases attachment");
            Ok(value)
        }
    }
}

/// Set-based counterpart of
/// [`replace_teslamate_import_projection_state_in_transaction`]. The source
/// was authenticated while attached read-only, so this writes one SQLite
/// `INSERT … SELECT` rather than crossing Rust for every source fact.
fn replace_teslamate_import_projection_state_from_attached_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    source_id: Uuid,
    base_snapshot_id: Uuid,
    head_sequence: u64,
    selected_car_id: i64,
    transfer: &TeslaMateProjectionStateTransfer,
    allow_create: bool,
) -> Result<(), StoreError> {
    if vehicle_id.is_nil()
        || source_id.is_nil()
        || base_snapshot_id.is_nil()
        || selected_car_id <= 0
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    let head_sequence = i64::try_from(head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let vehicle_key = vehicle_id.to_string();
    let base_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_bases AS base
                JOIN sync_heads AS head ON head.vehicle_id = base.vehicle_id
                 WHERE base.vehicle_id = ?1
                   AND base.snapshot_id = ?2
                   AND head.base_snapshot_id = base.snapshot_id
                   AND head.head_sequence = ?3
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                head_sequence
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !base_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let binding_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM v2_base_bindings
                 WHERE vehicle_id = ?1
                   AND snapshot_id = ?2
                   AND account_id = ?3
                   AND selected_car_id = ?4
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                source_id.to_string(),
                selected_car_id
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !binding_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let existing: Option<(String, String, i64)> = transaction
        .query_row(
            "SELECT source_id, base_snapshot_id, selected_car_id
               FROM teslamate_import_projection_state_heads WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if let Some((stored_source_id, stored_base_snapshot_id, stored_selected_car_id)) = existing {
        if stored_source_id != source_id.to_string()
            || stored_base_snapshot_id != base_snapshot_id.to_string()
            || stored_selected_car_id != selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
    } else if !allow_create {
        return Err(StoreError::TeslaMateImportProjectionStateMissing(
            vehicle_id,
        ));
    }
    transaction
        .execute(
            "INSERT INTO teslamate_import_projection_state_heads(
                vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(vehicle_id) DO UPDATE SET head_sequence = excluded.head_sequence",
            params![
                vehicle_key.as_str(),
                source_id.to_string(),
                base_snapshot_id.to_string(),
                selected_car_id,
                head_sequence,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "DELETE FROM teslamate_import_projection_state_rows WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    let inserted = transaction
        .execute(
            "INSERT INTO teslamate_import_projection_state_rows(
                vehicle_id, entity, entity_ordinal, entity_id, car_id, projection_sha256
             )
             SELECT ?1,
                    CASE entity_ordinal
                        WHEN 0 THEN 'car'
                        WHEN 1 THEN 'drive'
                        WHEN 2 THEN 'position'
                        WHEN 3 THEN 'charge'
                        WHEN 4 THEN 'charge_sample'
                        WHEN 5 THEN 'state'
                        WHEN 6 THEN 'update'
                    END,
                    entity_ordinal, entity_id, car_id, projection_sha256
               FROM teslamate_projection_state_spool.current_rows
              ORDER BY entity_ordinal ASC, entity_id ASC",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    if u64::try_from(inserted).map_err(|_| StoreError::LineageCatalogConflict)?
        != transfer.stats().row_count
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

/// Rebuild the retained legacy deletion inventory from the just-inserted
/// durable state. The car row has already been required by the descriptor and
/// intentionally remains absent from this legacy table. Reading the target
/// rather than the attachment makes the two catalogue views exactly identical
/// even if an outside process replaces the spool path after attachment.
fn replace_teslamate_import_projection_inventory_from_attached_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    source_id: Uuid,
    base_snapshot_id: Uuid,
    head_sequence: u64,
    selected_car_id: i64,
    transfer: &TeslaMateProjectionStateTransfer,
    allow_create: bool,
) -> Result<(), StoreError> {
    if vehicle_id.is_nil()
        || source_id.is_nil()
        || base_snapshot_id.is_nil()
        || selected_car_id <= 0
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    let head_sequence = i64::try_from(head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let vehicle_key = vehicle_id.to_string();
    let base_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_bases AS base
                JOIN sync_heads AS head ON head.vehicle_id = base.vehicle_id
                 WHERE base.vehicle_id = ?1
                   AND base.snapshot_id = ?2
                   AND head.base_snapshot_id = base.snapshot_id
                   AND head.head_sequence = ?3
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                head_sequence
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !base_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let existing: Option<(String, String, i64)> = transaction
        .query_row(
            "SELECT source_id, base_snapshot_id, selected_car_id
               FROM teslamate_import_projection_heads WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if let Some((stored_source_id, stored_base_snapshot_id, stored_selected_car_id)) = existing {
        if stored_source_id != source_id.to_string()
            || stored_base_snapshot_id != base_snapshot_id.to_string()
            || stored_selected_car_id != selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
    } else if !allow_create {
        return Err(StoreError::TeslaMateImportInventoryMissing(vehicle_id));
    }
    transaction
        .execute(
            "INSERT INTO teslamate_import_projection_heads(
                vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(vehicle_id) DO UPDATE SET head_sequence = excluded.head_sequence",
            params![
                vehicle_key.as_str(),
                source_id.to_string(),
                base_snapshot_id.to_string(),
                selected_car_id,
                head_sequence,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "DELETE FROM teslamate_import_projection_rows WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    let inserted = transaction
        .execute(
            "INSERT INTO teslamate_import_projection_rows(vehicle_id, entity, entity_id)
             SELECT vehicle_id, entity, entity_id
               FROM teslamate_import_projection_state_rows
              WHERE vehicle_id = ?1 AND entity_ordinal BETWEEN 1 AND 6
              ORDER BY entity_ordinal ASC, entity_id ASC",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    let expected = transfer
        .stats()
        .row_count
        .checked_sub(1)
        .ok_or(StoreError::LineageCatalogConflict)?;
    if u64::try_from(inserted).map_err(|_| StoreError::LineageCatalogConflict)? != expected {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

/// Replace the digest-only current projection state in the same transaction
/// that advances the immutable-base lineage head. `allow_create` is true only
/// while cataloguing an initial base; a successor must refuse missing state
/// rather than silently treating a legacy inventory as equivalent provenance.
pub(crate) fn replace_teslamate_import_projection_state_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    source_id: Uuid,
    base_snapshot_id: Uuid,
    head_sequence: u64,
    selected_car_id: i64,
    state: &TeslaMateProjectionState,
    allow_create: bool,
) -> Result<(), StoreError> {
    if vehicle_id.is_nil()
        || source_id.is_nil()
        || base_snapshot_id.is_nil()
        || selected_car_id <= 0
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    if !state.stats().sealed {
        return Err(StoreError::TeslaMateProjectionState(
            TeslaMateProjectionStateError::StateNotSealed,
        ));
    }
    let head_sequence = i64::try_from(head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let vehicle_key = vehicle_id.to_string();
    let base_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_bases AS base
                JOIN sync_heads AS head ON head.vehicle_id = base.vehicle_id
                 WHERE base.vehicle_id = ?1
                   AND base.snapshot_id = ?2
                   AND head.base_snapshot_id = base.snapshot_id
                   AND head.head_sequence = ?3
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                head_sequence
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !base_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let binding_matches: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM v2_base_bindings
                 WHERE vehicle_id = ?1
                   AND snapshot_id = ?2
                   AND account_id = ?3
                   AND selected_car_id = ?4
            )",
            params![
                vehicle_key.as_str(),
                base_snapshot_id.to_string(),
                source_id.to_string(),
                selected_car_id
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if !binding_matches {
        return Err(StoreError::LineageCatalogConflict);
    }
    let existing: Option<(String, String, i64)> = transaction
        .query_row(
            "SELECT source_id, base_snapshot_id, selected_car_id
               FROM teslamate_import_projection_state_heads WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if let Some((stored_source_id, stored_base_snapshot_id, stored_selected_car_id)) = existing {
        if stored_source_id != source_id.to_string()
            || stored_base_snapshot_id != base_snapshot_id.to_string()
            || stored_selected_car_id != selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
    } else if !allow_create {
        return Err(StoreError::TeslaMateImportProjectionStateMissing(
            vehicle_id,
        ));
    }
    transaction
        .execute(
            "INSERT INTO teslamate_import_projection_state_heads(
                vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(vehicle_id) DO UPDATE SET head_sequence = excluded.head_sequence",
            params![
                vehicle_key.as_str(),
                source_id.to_string(),
                base_snapshot_id.to_string(),
                selected_car_id,
                head_sequence,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "DELETE FROM teslamate_import_projection_state_rows WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LineageCatalog)?;
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO teslamate_import_projection_state_rows(
                vehicle_id, entity, entity_ordinal, entity_id, car_id, projection_sha256
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(StoreError::LineageCatalog)?;
    let mut saw_car = false;
    let mut after = None;
    loop {
        let page = state.page(after, MAX_PAGE_SIZE)?;
        for row in &page.rows {
            if row.id <= 0
                || row.car_id != selected_car_id
                || matches!(row.entity, TeslaMateProjectionStateEntity::Car)
                    && (row.id != selected_car_id || saw_car)
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            if matches!(row.entity, TeslaMateProjectionStateEntity::Car) {
                saw_car = true;
            }
            statement
                .execute(params![
                    vehicle_key.as_str(),
                    row.entity.as_str(),
                    i64::from(row.entity.ordinal()),
                    row.id,
                    row.car_id,
                    row.digest.as_bytes().as_slice(),
                ])
                .map_err(StoreError::LineageCatalog)?;
        }
        match page.next_after {
            Some(next_after) => after = Some(next_after),
            None => break,
        }
    }
    if !saw_car {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}
