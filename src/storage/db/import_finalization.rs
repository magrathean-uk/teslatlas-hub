// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    /// Publish a client-valid import successor under the immutable V2 base.
    ///
    /// The pack must already be a typed delta bound to the base snapshot ID and
    /// the half-open sequence `(from_exclusive, to_inclusive]`. Full-snapshot
    /// packs with a new snapshot identity are refused.
    pub fn finalize_import_delta_successor(
        &self,
        vehicle_id: Uuid,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    ) -> Result<(), StoreError> {
        self.finalize_import_delta_successor_with_inventory(
            vehicle_id,
            delta,
            cursor_key,
            terminal_cursor,
            fingerprint,
            geofences,
            None,
            None,
        )
    }

    /// As [`Self::finalize_import_delta_successor`], but atomically advances
    /// the source-owned TeslaMate history inventory with the lineage head.
    pub fn finalize_teslamate_import_delta_successor(
        &self,
        vehicle_id: Uuid,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        inventory: &TeslaMateImportProjectionInventory,
    ) -> Result<(), StoreError> {
        self.finalize_import_delta_successor_with_inventory(
            vehicle_id,
            delta,
            cursor_key,
            terminal_cursor,
            fingerprint,
            geofences,
            Some(inventory),
            None,
        )
    }

    /// As [`Self::finalize_teslamate_import_delta_successor`], but also
    /// atomically replaces the digest-only current projection state. This is
    /// the required completion path for a changed-history successor.
    pub fn finalize_teslamate_import_delta_successor_with_projection_state(
        &self,
        vehicle_id: Uuid,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        inventory: &TeslaMateImportProjectionInventory,
        projection_state: &TeslaMateProjectionState,
    ) -> Result<(), StoreError> {
        self.finalize_import_delta_successor_with_inventory(
            vehicle_id,
            delta,
            cursor_key,
            terminal_cursor,
            fingerprint,
            geofences,
            Some(inventory),
            Some(projection_state),
        )
    }

    /// Atomically publish every bounded direct-import successor produced from
    /// one sealed PostgreSQL snapshot. A source rewrite may exceed one pack,
    /// but clients must never observe only a prefix of its ordered deltas.
    /// `retain_legacy_inventory` is only for the older staged importer; the
    /// direct PostgreSQL path derives that view from digest state instead.
    pub fn finalize_import_generation_delta_successors_with_projection_state(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        deltas: &[LineageDelta],
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        projection_state: &TeslaMateProjectionState,
        retain_legacy_inventory: bool,
    ) -> Result<(), StoreError> {
        if run_id.is_nil() || source_id.is_nil() || vehicle_id.is_nil() || car_id <= 0 {
            return Err(StoreError::InvalidImportGeneration);
        }
        let Some(first_delta) = deltas.first() else {
            return Err(StoreError::LineageCatalogConflict);
        };
        let final_delta = deltas
            .last()
            .expect("first delta proves the successor batch is non-empty");
        let binding = self.v2_projection_binding(vehicle_id)?;
        // A direct import may only advance the immutable source/car binding
        // that created the base. Never let a caller reuse the vehicle UUID
        // with another source or selected TeslaMate car while replacing its
        // durable digest state and lifecycle session.
        if source_id != binding.account_id || car_id != binding.selected_car_id {
            return Err(StoreError::LineageCatalogConflict);
        }
        let existing = self
            .lineage_manifest_for_vehicle(vehicle_id)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        let mut prior_to = None;
        let mut prior_chain = None;
        let mut prior_ordinal = None;
        for delta in deltas {
            validate_import_delta_successor_shape(delta)?;
            self.verify_import_delta_pack(delta, &binding)?;
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
        let cursor_claims = terminal_cursor
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
        // Validate the exact post-commit lineage before writing any catalogue
        // rows. This enforces the aggregate row/byte/pack ceilings across the
        // existing immutable base plus this whole bounded batch, rather than
        // merely checking every delta in isolation.
        let mut candidate_lineage = existing;
        candidate_lineage.deltas.extend_from_slice(deltas);
        candidate_lineage.head_sequence = final_delta.to_sequence;
        candidate_lineage.head_digest = final_delta.chain_digest;
        candidate_lineage.terminal_cursor = terminal_cursor.clone();
        candidate_lineage
            .validate_with_limits(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        let transfer = projection_state
            .sealed_transfer_for_import_generation(run_id, binding.selected_car_id)?;
        let terminal_cursor_json =
            serde_json::to_string(terminal_cursor).map_err(StoreError::SerializeManifest)?;
        let vehicle_key = vehicle_id.to_string();
        let mut connection = self.open()?;
        attach_teslamate_projection_state_transfer(&connection, &transfer)?;
        let result = (|| -> Result<(), StoreError> {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(StoreError::Begin)?;
            let (encoded, base_last_observation_id, base_updated_at_ms): (String, i64, i64) =
                transaction
                    .query_row(
                        "SELECT sessions.session_json, generations.base_last_observation_id,
                        generations.base_updated_at_ms
                 FROM import_generation_sessions AS sessions
                 JOIN import_generations AS generations USING(run_id)
                 WHERE generations.run_id = ?1 AND generations.source_id = ?2
                   AND generations.vehicle_id = ?3 AND generations.car_id = ?4
                   AND generations.status = 'staging'",
                        params![
                            run_id.to_string(),
                            source_id.to_string(),
                            vehicle_key.as_str(),
                            car_id
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(StoreError::ImportGeneration)?
                    .ok_or(StoreError::ImportGenerationNotFound)?;
            let session =
                serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
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
                insert_import_delta_in_transaction(&transaction, &vehicle_key, delta)?;
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
            replace_teslamate_import_projection_state_from_attached_in_transaction(
                &transaction,
                vehicle_id,
                binding.account_id,
                final_delta.pack.snapshot_id,
                final_delta.to_sequence,
                binding.selected_car_id,
                &transfer,
                false,
            )?;
            if retain_legacy_inventory {
                replace_teslamate_import_projection_inventory_from_attached_in_transaction(
                    &transaction,
                    vehicle_id,
                    binding.account_id,
                    final_delta.pack.snapshot_id,
                    final_delta.to_sequence,
                    binding.selected_car_id,
                    &transfer,
                    false,
                )?;
            }
            promote_imported_open_session_in_transaction(
                &transaction,
                source_id,
                vehicle_id,
                car_id,
                &session,
                updated_at_ms,
                Some((base_last_observation_id, base_updated_at_ms)),
            )?;
            upsert_geofences_in_transaction(&transaction, vehicle_id, geofences)?;
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
            self.commit_catalogue_receipted_transaction(
                transaction,
                "import_generation_delta_batch",
                vehicle_id,
                final_delta.pack.snapshot_id,
                StoreError::ImportGeneration,
            )
        })();
        finish_teslamate_projection_state_transfer(
            result,
            detach_teslamate_projection_state_transfer(self, &connection),
        )
    }

    fn finalize_import_delta_successor_with_inventory(
        &self,
        vehicle_id: Uuid,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        inventory: Option<&TeslaMateImportProjectionInventory>,
        projection_state: Option<&TeslaMateProjectionState>,
    ) -> Result<(), StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        delta
            .pack
            .validate(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        if delta.pack.snapshot_id.is_nil()
            || delta.from_sequence >= delta.to_sequence
            || delta.pack_digest != delta.pack.sha256
            || delta.pack.schema != HUB_PROJECTION_SCHEMA_V2
            || delta.pack.format != crate::protocol::PackFormat::HubProjectionSqlite
            || delta.pack.compression != crate::protocol::PackCompression::Zstd
            || delta.pack.sequence
                != (SequenceRange {
                    from_exclusive: delta.from_sequence,
                    to_inclusive: delta.to_sequence,
                })
            || delta.chain_digest
                != canonical_delta_chain_digest(delta.parent_chain_digest, delta.pack.sha256)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let binding = self.v2_projection_binding(vehicle_id)?;
        self.verify_import_delta_pack(delta, &binding)?;
        if let Some(inventory) = inventory {
            validate_teslamate_import_delta_inventory(delta, &binding, inventory)?;
        }
        if projection_state.is_some() && inventory.is_none() {
            return Err(StoreError::LineageCatalogConflict);
        }
        let cursor_claims = terminal_cursor
            .verify(cursor_key)
            .map_err(StoreError::Manifest)?;
        if cursor_claims.protocol != crate::protocol::PROTOCOL_V1
            || cursor_claims.schema != HUB_PROJECTION_SCHEMA_V2
            || cursor_claims.installation_id != binding.installation_id
            || cursor_claims.account_id != binding.account_id
            || cursor_claims.vehicle_id != binding.vehicle_id
            || cursor_claims.generation != binding.generation
            || cursor_claims.sequence != delta.to_sequence
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let terminal_cursor_json =
            serde_json::to_string(terminal_cursor).map_err(StoreError::SerializeManifest)?;
        let vehicle_key = vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let base_snapshot: String = transaction
            .query_row(
                "SELECT snapshot_id FROM sync_bases WHERE vehicle_id = ?1",
                params![vehicle_key.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        if base_snapshot != delta.pack.snapshot_id.to_string() {
            return Err(StoreError::LineageCatalogConflict);
        }
        let current: Option<(i64, String)> = transaction
            .query_row(
                "SELECT head_sequence, head_digest FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((head_sequence, head_digest)) = current else {
            return Err(StoreError::LineageCatalogConflict);
        };
        if head_sequence
            != i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?
            || head_digest != delta.parent_chain_digest.to_string()
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let pack_json = serde_json::to_vec(delta).map_err(StoreError::SerializeManifest)?;
        let inserted = transaction
            .execute(
                "INSERT INTO sync_deltas(
                    vehicle_id, from_sequence, to_sequence, parent_chain_digest,
                    chain_digest, pack_digest, pack_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(vehicle_id, from_sequence, to_sequence) DO NOTHING",
                params![
                    vehicle_key.as_str(),
                    i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    delta.parent_chain_digest.to_string(),
                    delta.chain_digest.to_string(),
                    delta.pack_digest.to_string(),
                    pack_json,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if inserted != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        Self::register_lineage_pack_snapshot(
            &transaction,
            &vehicle_key,
            &delta.pack,
            delta.to_sequence,
            &pack_json,
        )?;
        let existing_pack: Option<(String, i64, String, i64, i64)> = transaction
            .query_row(
                "SELECT snapshot_id, ordinal, relative_path,
                        compressed_bytes, uncompressed_bytes
                 FROM sync_packs WHERE sha256 = ?1",
                params![delta.pack.sha256.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if let Some((snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes)) =
            existing_pack
        {
            if snapshot_id != delta.pack.snapshot_id.to_string()
                || ordinal != i64::from(delta.pack.ordinal)
                || relative_path != delta.pack.relative_path
                || compressed_bytes
                    != i64::try_from(delta.pack.compressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?
                || uncompressed_bytes
                    != i64::try_from(delta.pack.uncompressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?
            {
                return Err(StoreError::LineageCatalogConflict);
            }
        } else {
            let occupied: Option<String> = transaction
                .query_row(
                    "SELECT sha256 FROM sync_packs
                     WHERE snapshot_id = ?1 AND ordinal = ?2",
                    params![
                        delta.pack.snapshot_id.to_string(),
                        i64::from(delta.pack.ordinal)
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?;
            if occupied.is_some() {
                return Err(StoreError::LineageCatalogConflict);
            }
            transaction
                .execute(
                    "INSERT INTO sync_packs(
                        sha256, snapshot_id, ordinal, relative_path,
                        compressed_bytes, uncompressed_bytes
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        delta.pack.sha256.to_string(),
                        delta.pack.snapshot_id.to_string(),
                        i64::from(delta.pack.ordinal),
                        delta.pack.relative_path,
                        i64::try_from(delta.pack.compressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                        i64::try_from(delta.pack.uncompressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }
        let updated = transaction
            .execute(
                "UPDATE sync_heads SET head_sequence = ?1, head_digest = ?2,
                        terminal_cursor = ?3
                 WHERE vehicle_id = ?4 AND head_sequence = ?5
                   AND head_digest = ?6",
                params![
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    delta.chain_digest.to_string(),
                    terminal_cursor_json,
                    vehicle_key.as_str(),
                    head_sequence,
                    head_digest,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if updated != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        if let Some(inventory) = inventory {
            replace_teslamate_import_inventory_in_transaction(
                &transaction,
                vehicle_id,
                delta.pack.snapshot_id,
                delta.to_sequence,
                inventory,
                false,
            )?;
        }
        if let Some(projection_state) = projection_state {
            replace_teslamate_import_projection_state_in_transaction(
                &transaction,
                vehicle_id,
                binding.account_id,
                delta.pack.snapshot_id,
                delta.to_sequence,
                binding.selected_car_id,
                projection_state,
                false,
            )?;
        }
        upsert_geofences_in_transaction(&transaction, vehicle_id, geofences)?;
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
                    delta.pack.snapshot_id.to_string(),
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                ],
            )
            .map_err(StoreError::PublishManifest)?;
        self.commit_catalogue_receipted_transaction(
            transaction,
            "import_delta_successor",
            vehicle_id,
            delta.pack.snapshot_id,
            StoreError::LineageCatalog,
        )
    }

    /// Load the exact source-owned history rows from the most recent
    /// successful TeslaMate import. New direct imports derive this legacy
    /// view from their durable digest state; older imports retain the original
    /// inventory table. Missing or mismatched provenance is a hard failure:
    /// callers must not guess deletes from mutable history.
    pub fn teslamate_import_projection_inventory(
        &self,
        vehicle_id: Uuid,
        source_id: Uuid,
        selected_car_id: i64,
    ) -> Result<TeslaMateImportProjectionInventory, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if source_id.is_nil() || selected_car_id <= 0 {
            return Err(StoreError::LineageCatalogConflict);
        }
        let connection = self.open()?;
        let legacy_header: Option<(String, String, i64)> = connection
            .query_row(
                "SELECT source_id, base_snapshot_id, selected_car_id
                   FROM teslamate_import_projection_heads WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let (stored_source_id, base_snapshot_id, stored_selected_car_id, use_digest_state) =
            if let Some((stored_source_id, base_snapshot_id, stored_selected_car_id)) =
                legacy_header
            {
                (
                    stored_source_id,
                    base_snapshot_id,
                    stored_selected_car_id,
                    false,
                )
            } else {
                let state_header: Option<(String, String, i64)> = connection
                    .query_row(
                        "SELECT source_id, base_snapshot_id, selected_car_id
                           FROM teslamate_import_projection_state_heads
                          WHERE vehicle_id = ?1",
                        params![vehicle_id.to_string()],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(StoreError::LineageCatalog)?;
                let Some((stored_source_id, base_snapshot_id, stored_selected_car_id)) =
                    state_header
                else {
                    return Err(StoreError::TeslaMateImportInventoryMissing(vehicle_id));
                };
                (
                    stored_source_id,
                    base_snapshot_id,
                    stored_selected_car_id,
                    true,
                )
            };
        if stored_source_id != source_id.to_string()
            || stored_selected_car_id != selected_car_id
            || !connection
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM sync_bases
                         WHERE vehicle_id = ?1 AND snapshot_id = ?2
                    )",
                    params![vehicle_id.to_string(), base_snapshot_id],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(StoreError::LineageCatalog)?
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let query = if use_digest_state {
            "SELECT entity, entity_id
               FROM teslamate_import_projection_state_rows
              WHERE vehicle_id = ?1 AND entity_ordinal BETWEEN 1 AND 6
              ORDER BY entity, entity_id"
        } else {
            "SELECT entity, entity_id
               FROM teslamate_import_projection_rows
              WHERE vehicle_id = ?1
              ORDER BY entity, entity_id"
        };
        let mut statement = connection
            .prepare(query)
            .map_err(StoreError::LineageCatalog)?;
        let rows = statement
            .query_map(params![vehicle_id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(StoreError::LineageCatalog)?;
        let mut inventory_rows = Vec::new();
        for row in rows {
            let (entity, id) = row.map_err(StoreError::LineageCatalog)?;
            inventory_rows.push(ProjectionTombstone {
                entity: teslamate_inventory_entity(&entity)
                    .ok_or(StoreError::LineageCatalogConflict)?,
                id,
                car_id: selected_car_id,
            });
        }
        validate_teslamate_import_inventory_rows(selected_car_id, &inventory_rows)?;
        Ok(TeslaMateImportProjectionInventory {
            source_id,
            selected_car_id,
            rows: inventory_rows,
        })
    }

    /// Open a bounded digest lookup for a verified prior TeslaMate import.
    /// A legacy deletion inventory is not a substitute: this deliberately
    /// fails if the separate durable digest state was never persisted.
    pub fn teslamate_import_projection_state_lookup(
        &self,
        vehicle_id: Uuid,
        source_id: Uuid,
        selected_car_id: i64,
    ) -> Result<TeslaMateImportProjectionStateLookup, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if source_id.is_nil() || selected_car_id <= 0 {
            return Err(StoreError::LineageCatalogConflict);
        }
        let connection = self.open_read_only_connection()?;
        // Keep every digest lookup/page on one SQLite read snapshot. If a
        // publisher advances the lineage concurrently, the later atomic
        // finalizer revalidates its head and refuses this stale capture.
        connection
            .execute_batch("BEGIN")
            .map_err(StoreError::Begin)?;
        let header: Option<(String, String, i64, i64)> = connection
            .query_row(
                "SELECT source_id, base_snapshot_id, selected_car_id, head_sequence
                   FROM teslamate_import_projection_state_heads
                  WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((
            stored_source_id,
            stored_base_snapshot_id,
            stored_selected_car_id,
            head_sequence,
        )) = header
        else {
            return Err(StoreError::TeslaMateImportProjectionStateMissing(
                vehicle_id,
            ));
        };
        let stored_source_id = Uuid::parse_str(&stored_source_id)
            .map_err(|_| StoreError::InvalidStoredUuid("projection-state source id"))?;
        let base_snapshot_id = Uuid::parse_str(&stored_base_snapshot_id)
            .map_err(|_| StoreError::InvalidStoredUuid("projection-state base snapshot id"))?;
        let head_sequence =
            u64::try_from(head_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
        let header = TeslaMateImportProjectionStateHeader {
            source_id: stored_source_id,
            base_snapshot_id,
            selected_car_id: stored_selected_car_id,
            head_sequence,
        };
        if header.source_id != source_id || header.selected_car_id != selected_car_id {
            return Err(StoreError::LineageCatalogConflict);
        }
        let current: Option<(String, i64)> = connection
            .query_row(
                "SELECT base.snapshot_id, head.head_sequence
                   FROM sync_bases AS base
                   JOIN sync_heads AS head
                     ON head.vehicle_id = base.vehicle_id
                    AND head.base_snapshot_id = base.snapshot_id
                  WHERE base.vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((current_base_snapshot_id, current_head_sequence)) = current else {
            return Err(StoreError::LineageCatalogConflict);
        };
        let current_head_sequence =
            u64::try_from(current_head_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
        // The projection-state head records the last completed TeslaMate
        // import. Normal Hub collection may advance the same immutable
        // lineage afterward. Reject a different base or a regressed head;
        // the atomic import finalizer revalidates the newer live head before
        // publishing a successor.
        if current_base_snapshot_id != header.base_snapshot_id.to_string()
            || current_head_sequence < header.head_sequence
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let binding: Option<(String, String, i64)> = connection
            .query_row(
                "SELECT snapshot_id, account_id, selected_car_id
                   FROM v2_base_bindings
                  WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((binding_base_snapshot_id, binding_account_id, binding_selected_car_id)) = binding
        else {
            return Err(StoreError::ImmutableBaseBindingMissing(vehicle_id));
        };
        if binding_base_snapshot_id != header.base_snapshot_id.to_string()
            || binding_account_id != header.source_id.to_string()
            || binding_selected_car_id != header.selected_car_id
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        Ok(TeslaMateImportProjectionStateLookup {
            connection,
            vehicle_id,
            header,
            digest_caches: Vec::new(),
            #[cfg(test)]
            digest_cache_loads: 0,
        })
    }

    /// Read one ordered bounded page of a verified prior state without
    /// exposing its SQLite connection to the caller.
    pub fn teslamate_import_projection_state_page(
        &self,
        vehicle_id: Uuid,
        source_id: Uuid,
        selected_car_id: i64,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<TeslaMateProjectionStateDigestPage, StoreError> {
        let mut lookup =
            self.teslamate_import_projection_state_lookup(vehicle_id, source_id, selected_car_id)?;
        lookup.page_after_store(after, limit)
    }

    /// Whether this V2 vehicle already has a durable projection-state head.
    /// The direct importer uses this narrow predicate only to decide whether
    /// an old inventory-only base may attempt the one-time bridge; full
    /// binding and head validation remains in the lookup/finalizer paths.
    pub(crate) fn teslamate_import_projection_state_exists(
        &self,
        vehicle_id: Uuid,
    ) -> Result<bool, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        self.open()?
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM teslamate_import_projection_state_heads
                     WHERE vehicle_id = ?1
                )",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)
    }

    /// True only for the exact legacy direct-import shape that can be safely
    /// upgraded in place. Anything less precise must use an owner-approved
    /// rebase rather than guessing a sparse successor.
    pub(crate) fn legacy_teslamate_direct_bridge_is_eligible(
        &self,
        vehicle_id: Uuid,
        source_id: Uuid,
        selected_car_id: i64,
    ) -> Result<bool, StoreError> {
        let connection = self.open()?;
        Ok(
            legacy_direct_bridge_candidate(&connection, vehicle_id, source_id, selected_car_id)?
                .is_some(),
        )
    }

    /// Atomically attach a sealed digest state to one verified legacy direct
    /// base and replace its retired physical fingerprint with the current
    /// fragment-independent logical fingerprint. This never publishes a pack,
    /// delta, or sequence. Any failed compatibility check rolls all writes
    /// back and reports that a rebase is required.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bridge_legacy_teslamate_direct_import(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        selected_car_id: i64,
        legacy_fingerprint: Sha256Digest,
        logical_fingerprint: Sha256Digest,
        projection_state: &TeslaMateProjectionState,
    ) -> Result<TeslaMateLegacyDirectBridgeResult, StoreError> {
        if run_id.is_nil() || source_id.is_nil() || vehicle_id.is_nil() || selected_car_id <= 0 {
            return Err(StoreError::InvalidImportGeneration);
        }
        let transfer = projection_state
            .sealed_transfer_for_import_generation(run_id, selected_car_id)
            .map_err(|error| {
                legacy_direct_bridge_state_error(
                    vehicle_id,
                    StoreError::TeslaMateProjectionState(error),
                )
            })?;
        let mut connection = self.open()?;
        attach_teslamate_projection_state_transfer(&connection, &transfer)?;
        let result = (|| -> Result<TeslaMateLegacyDirectBridgeResult, StoreError> {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(StoreError::Begin)?;
            let generation_is_staging: bool = transaction
                .query_row(
                    "SELECT EXISTS(
                    SELECT 1 FROM import_generations
                     WHERE run_id = ?1 AND source_id = ?2 AND vehicle_id = ?3
                       AND car_id = ?4 AND status = 'staging'
                )",
                    params![
                        run_id.to_string(),
                        source_id.to_string(),
                        vehicle_id.to_string(),
                        selected_car_id,
                    ],
                    |row| row.get(0),
                )
                .map_err(StoreError::ImportGeneration)?;
            if !generation_is_staging {
                return Err(StoreError::ImportGenerationNotFound);
            }
            let candidate = legacy_direct_bridge_candidate(
                &transaction,
                vehicle_id,
                source_id,
                selected_car_id,
            )?
            .ok_or(StoreError::TeslaMateLegacyDirectRebaseRequired(vehicle_id))?;
            if candidate.legacy_fingerprint != legacy_fingerprint {
                return Err(StoreError::TeslaMateLegacyDirectRebaseRequired(vehicle_id));
            }
            replace_teslamate_import_projection_state_from_attached_in_transaction(
                &transaction,
                vehicle_id,
                source_id,
                candidate.snapshot_id,
                candidate.head_sequence,
                selected_car_id,
                &transfer,
                true,
            )
            .map_err(|error| legacy_direct_bridge_state_error(vehicle_id, error))?;
            if !legacy_projection_inventory_matches_state_in_transaction(&transaction, vehicle_id)?
            {
                return Err(StoreError::TeslaMateLegacyDirectRebaseRequired(vehicle_id));
            }
            if transaction
                .execute(
                    "UPDATE snapshot_fingerprints
                    SET fingerprint_sha256 = ?1
                  WHERE vehicle_id = ?2
                    AND fingerprint_sha256 = ?3
                    AND snapshot_id = ?4
                    AND head_sequence = ?5",
                    params![
                        logical_fingerprint.as_bytes().as_slice(),
                        vehicle_id.to_string(),
                        legacy_fingerprint.as_bytes().as_slice(),
                        candidate.snapshot_id.to_string(),
                        i64::try_from(candidate.head_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                    ],
                )
                .map_err(StoreError::PublishManifest)?
                != 1
            {
                return Err(StoreError::TeslaMateLegacyDirectRebaseRequired(vehicle_id));
            }
            transaction
                .execute(
                    "INSERT INTO teslamate_import_projection_state_bridges(
                    vehicle_id, base_snapshot_id, head_sequence, algorithm,
                    legacy_fingerprint_sha256, logical_fingerprint_sha256
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        vehicle_id.to_string(),
                        candidate.snapshot_id.to_string(),
                        i64::try_from(candidate.head_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        TESLAMATE_LEGACY_DIRECT_BRIDGE_ALGORITHM,
                        legacy_fingerprint.as_bytes().as_slice(),
                        logical_fingerprint.as_bytes().as_slice(),
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
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
            transaction.commit().map_err(StoreError::ImportGeneration)?;
            Ok(TeslaMateLegacyDirectBridgeResult {
                snapshot_id: candidate.snapshot_id,
                head_sequence: candidate.head_sequence,
                total_rows: candidate.total_rows,
            })
        })();
        finish_teslamate_projection_state_transfer(
            result,
            detach_teslamate_projection_state_transfer(self, &connection),
        )
    }

    /// True when the vehicle's current source fingerprint equals `fingerprint`.
    /// Unlike [`Self::manifest_for_snapshot_fingerprint`], this does not require
    /// a legacy full-snapshot `sync_manifests` row, so import deltas can skip.
    pub fn source_fingerprint_matches(
        &self,
        vehicle_id: Uuid,
        fingerprint: Sha256Digest,
    ) -> Result<bool, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM snapshot_fingerprints
                     WHERE vehicle_id = ?1 AND fingerprint_sha256 = ?2
                )",
                params![vehicle_id.to_string(), fingerprint.as_bytes().as_slice()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    pub fn manifest_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<SyncManifest>, StoreError> {
        let connection = self.open()?;
        let payload = connection
            .query_row(
                "SELECT manifest_json FROM sync_manifests \
                 WHERE vehicle_id = ?1
                   AND json_extract(manifest_json, '$.mode') = 'full_snapshot'
                 ORDER BY head_sequence DESC LIMIT 1",
                params![vehicle_id.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        payload.map(decode_manifest).transpose()
    }

    /// Load the exact manifest atomically associated with a source snapshot
    /// fingerprint. Legacy unbound fingerprints deliberately return `None`.
    pub fn manifest_for_snapshot_fingerprint(
        &self,
        vehicle_id: Uuid,
        fingerprint: Sha256Digest,
    ) -> Result<Option<SyncManifest>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let payload = connection
            .query_row(
                "SELECT manifests.manifest_json
                 FROM snapshot_fingerprints AS fingerprints
                 JOIN sync_manifests AS manifests
                   ON manifests.snapshot_id = fingerprints.snapshot_id
                  AND manifests.vehicle_id = fingerprints.vehicle_id
                  AND manifests.head_sequence = fingerprints.head_sequence
                 WHERE fingerprints.vehicle_id = ?1
                   AND fingerprints.fingerprint_sha256 = ?2",
                params![vehicle_id.to_string(), fingerprint.as_bytes().as_slice()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        payload.map(decode_manifest).transpose()
    }

    pub fn snapshot_fingerprint_is_current(
        &self,
        vehicle_id: Uuid,
        fingerprint: Sha256Digest,
    ) -> Result<bool, StoreError> {
        Ok(self
            .manifest_for_snapshot_fingerprint(vehicle_id, fingerprint)?
            .is_some())
    }

    /// Whether a pack is still authorized by any live catalogue row or by a
    /// retired lineage inside its physical-delete grace window. Candidate
    /// cleanup must use this stronger predicate: a retired client may still
    /// be reading an immutable object after it leaves `sync_packs`.
    #[cfg(test)]
    pub(crate) fn pack_sha256_is_retained(&self, sha256: &str) -> Result<bool, StoreError> {
        let now_ms = retired_lineage_clock_ms()?;
        let cutoff_ms = now_ms.saturating_sub(RETIRED_LINEAGE_PACK_DELETE_GRACE_MS);
        let connection = self.open()?;
        Ok(referenced_pack_rows_at(&connection, cutoff_ms)?
            .iter()
            .any(|(candidate, _, _)| candidate == sha256))
    }

    /// Remove one newly-created but unpublished content object while the
    /// caller owns the cross-process publication gate. The SQLite immediate
    /// transaction also blocks older ungated catalogue writers for the whole
    /// retained-reference check and unlink. Path traversal is descriptor
    /// relative and the admitted inode must still match after hashing.
    pub(crate) fn remove_unretained_pack(
        &self,
        _publication_gate: &PublicationGate,
        sha256: Sha256Digest,
        candidate_path: &Path,
    ) -> Result<PackCleanupOutcome, StoreError> {
        let digest = sha256.to_string();
        let file_name = format!("{digest}.sqlite.zst");
        let content_dir_path = self.packs_dir.join("sha256");
        let expected_path = content_dir_path.join(&file_name);
        if candidate_path != expected_path {
            return Err(StoreError::UnsafeUnpublishedPackPath(
                candidate_path.to_path_buf(),
            ));
        }

        let cutoff_ms =
            retired_lineage_clock_ms()?.saturating_sub(RETIRED_LINEAGE_PACK_DELETE_GRACE_MS);
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if referenced_pack_rows_at(&transaction, cutoff_ms)?
            .iter()
            .any(|(candidate, _, _)| candidate == &digest)
        {
            transaction.commit().map_err(StoreError::LineageCatalog)?;
            return Ok(PackCleanupOutcome::Retained);
        }

        let content_directory =
            open_directory_path_nofollow(&content_dir_path).map_err(|source| {
                StoreError::CleanupUnpublishedPack {
                    path: content_dir_path.clone(),
                    source,
                }
            })?;
        let before = match statat(
            &content_directory,
            file_name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(value) => value,
            Err(Errno::NOENT) => {
                transaction.commit().map_err(StoreError::LineageCatalog)?;
                return Ok(PackCleanupOutcome::Missing);
            }
            Err(source) => {
                return Err(StoreError::CleanupUnpublishedPack {
                    path: expected_path,
                    source: source.into(),
                });
            }
        };
        if !FileType::from_raw_mode(before.st_mode).is_file()
            || before.st_uid != rustix::process::geteuid().as_raw()
            || stat_mode(before.st_mode) != 0o640
            || !(1..=2).contains(&before.st_nlink)
        {
            return Err(StoreError::UnsafeUnpublishedPackPath(expected_path));
        }
        let descriptor = openat(
            &content_directory,
            file_name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|source| StoreError::CleanupUnpublishedPack {
            path: expected_path.clone(),
            source: source.into(),
        })?;
        let mut file = File::from(descriptor);
        let opened = fstat(&file).map_err(|source| StoreError::CleanupUnpublishedPack {
            path: expected_path.clone(),
            source: source.into(),
        })?;
        if opened.st_dev != before.st_dev
            || opened.st_ino != before.st_ino
            || opened.st_size != before.st_size
            || opened.st_mode != before.st_mode
            || opened.st_uid != before.st_uid
            || opened.st_gid != before.st_gid
            || opened.st_nlink != before.st_nlink
        {
            return Err(StoreError::UnsafeUnpublishedPackPath(expected_path));
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read =
                file.read(&mut buffer)
                    .map_err(|source| StoreError::CleanupUnpublishedPack {
                        path: expected_path.clone(),
                        source,
                    })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        if hex::encode(hasher.finalize()) != digest {
            return Err(StoreError::UnpublishedPackDigestMismatch(expected_path));
        }
        let after = statat(
            &content_directory,
            file_name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|source| StoreError::CleanupUnpublishedPack {
            path: expected_path.clone(),
            source: source.into(),
        })?;
        if after.st_dev != opened.st_dev
            || after.st_ino != opened.st_ino
            || after.st_size != opened.st_size
            || after.st_mode != opened.st_mode
            || after.st_uid != opened.st_uid
            || after.st_gid != opened.st_gid
            || after.st_nlink != opened.st_nlink
        {
            return Err(StoreError::UnsafeUnpublishedPackPath(expected_path));
        }
        unlinkat(&content_directory, file_name.as_str(), AtFlags::empty()).map_err(|source| {
            StoreError::CleanupUnpublishedPack {
                path: expected_path.clone(),
                source: source.into(),
            }
        })?;
        content_directory
            .sync_all()
            .map_err(|source| StoreError::CleanupUnpublishedPack {
                path: expected_path,
                source,
            })?;
        transaction.commit().map_err(StoreError::LineageCatalog)?;
        Ok(PackCleanupOutcome::Removed)
    }

    pub fn record_snapshot_fingerprint(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
    ) -> Result<(), StoreError> {
        if manifest.vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
        transaction.commit().map_err(StoreError::PublishManifest)
    }

    /// Reserve a full-snapshot marker while the caller owns the publication
    /// gate. Other modules cannot reserve a marker without this token.
    pub(crate) fn reserve_next_full_snapshot_sequence(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
    ) -> Result<u64, StoreError> {
        self.next_full_snapshot_sequence_while_gated(vehicle_id)
    }

    /// Durably reserve the next full-snapshot marker while owning the
    /// publication gate for this single reservation.
    ///
    /// This compatibility seam keeps callers from bypassing publication
    /// serialization without exposing the gate token itself. Workflows that
    /// already hold the gate should use `reserve_next_full_snapshot_sequence`.
    pub fn next_full_snapshot_sequence(&self, vehicle_id: Uuid) -> Result<u64, StoreError> {
        let publication_gate = self.try_acquire_publication_gate()?;
        self.reserve_next_full_snapshot_sequence(&publication_gate, vehicle_id)
    }

    /// Durably reserve the next full-snapshot marker for one Hub vehicle.
    ///
    /// Reservation happens before pack construction, so a failed unpublished
    /// build can leave a harmless gap. It cannot reuse a marker already handed
    /// to another process, keeping successful publications totally ordered.
    fn next_full_snapshot_sequence_while_gated(&self, vehicle_id: Uuid) -> Result<u64, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        // The counter records reservations, while the catalogue can also be
        // advanced by a live publisher. The caller holds the publication gate.
        let next_counter: Option<i64> = transaction
            .query_row(
                "SELECT next_sequence FROM vehicle_snapshot_sequences WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let catalog_head: Option<i64> = transaction
            .query_row(
                "SELECT MAX(head_sequence) FROM (
                    SELECT head_sequence FROM sync_manifests WHERE vehicle_id = ?1
                    UNION ALL
                    SELECT head_sequence FROM sync_heads WHERE vehicle_id = ?1
                 )",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let reserved = catalog_head
            .unwrap_or(0)
            .max(next_counter.unwrap_or(1).saturating_sub(1))
            .checked_add(1)
            .ok_or(StoreError::SequenceExhausted)?;
        let next_sequence = reserved
            .checked_add(1)
            .ok_or(StoreError::SequenceExhausted)?;
        transaction
            .execute(
                "INSERT INTO vehicle_snapshot_sequences (vehicle_id, next_sequence)
                 VALUES (?1, ?2)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    next_sequence = MAX(vehicle_snapshot_sequences.next_sequence, excluded.next_sequence)",
                params![vehicle_id.to_string(), next_sequence],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::Query)?;
        u64::try_from(reserved)
            .ok()
            .filter(|sequence| *sequence >= 1)
            .ok_or(StoreError::SequenceExhausted)
    }

    /// Make the pack catalogue, imported lifecycle recovery state, geofences,
    /// fingerprint, and staging cleanup visible in one SQLite transaction.
    /// Callers retain immutable pack chunks before this transaction starts.
    pub fn finalize_import_generation(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    ) -> Result<(), StoreError> {
        self.finalize_import_generation_with_metadata(
            run_id,
            source_id,
            vehicle_id,
            car_id,
            updated_at_ms,
            manifest,
            fingerprint,
            geofences,
            None,
        )
    }

    /// Finalize an imported V2 base while retaining the exact immutable
    /// projection binding that wrote it.
    pub fn finalize_import_generation_with_binding(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: &ProjectionBinding,
    ) -> Result<(), StoreError> {
        self.finalize_import_generation_with_metadata(
            run_id,
            source_id,
            vehicle_id,
            car_id,
            updated_at_ms,
            manifest,
            fingerprint,
            geofences,
            Some(binding),
        )
    }

    /// Finalize a direct TeslaMate V2 base together with its sealed,
    /// digest-only state. That state is the sole current-history catalogue:
    /// legacy deletion inventory requests derive their non-car view from it
    /// instead of retaining a second multi-million-row copy. Set
    /// `retain_legacy_inventory` only for the older staged importer.
    pub fn finalize_import_generation_with_projection_state(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: &ProjectionBinding,
        projection_state: &TeslaMateProjectionState,
        retain_legacy_inventory: bool,
    ) -> Result<(), StoreError> {
        if run_id.is_nil()
            || source_id.is_nil()
            || vehicle_id.is_nil()
            || car_id <= 0
            || manifest.vehicle_id != vehicle_id
        {
            return Err(StoreError::InvalidImportGeneration);
        }
        validate_immutable_v2_base_binding(manifest, binding)?;
        // The staging generation feeds the imported lifecycle materialisation,
        // while the V2 binding owns the pack/state scope.  They must describe
        // the exact same source/car; otherwise a valid selected-car base could
        // be published next to lifecycle rows attributed to another car.
        if source_id != binding.account_id || car_id != binding.selected_car_id {
            return Err(StoreError::LineageCatalogConflict);
        }
        let transfer = projection_state
            .sealed_transfer_for_import_generation(run_id, binding.selected_car_id)?;
        let mut connection = self.open()?;
        attach_teslamate_projection_state_transfer(&connection, &transfer)?;
        let result = (|| -> Result<(), StoreError> {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(StoreError::Begin)?;
            let (encoded, base_last_observation_id, base_updated_at_ms): (String, i64, i64) =
                transaction
                    .query_row(
                        "SELECT sessions.session_json, generations.base_last_observation_id,
                            generations.base_updated_at_ms
                     FROM import_generation_sessions AS sessions
                     JOIN import_generations AS generations USING(run_id)
                     WHERE generations.run_id = ?1 AND generations.source_id = ?2
                       AND generations.vehicle_id = ?3 AND generations.car_id = ?4
                       AND generations.status = 'staging'",
                        params![
                            run_id.to_string(),
                            source_id.to_string(),
                            vehicle_id.to_string(),
                            car_id
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(StoreError::ImportGeneration)?
                    .ok_or(StoreError::ImportGenerationNotFound)?;
            let session =
                serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
            publish_manifest_in_transaction(&transaction, manifest, Some(binding))?;
            promote_imported_open_session_in_transaction(
                &transaction,
                source_id,
                vehicle_id,
                car_id,
                &session,
                updated_at_ms,
                Some((base_last_observation_id, base_updated_at_ms)),
            )?;
            replace_teslamate_import_projection_state_from_attached_in_transaction(
                &transaction,
                vehicle_id,
                binding.account_id,
                manifest.snapshot_id,
                manifest.head_sequence,
                binding.selected_car_id,
                &transfer,
                true,
            )?;
            if retain_legacy_inventory {
                replace_teslamate_import_projection_inventory_from_attached_in_transaction(
                    &transaction,
                    vehicle_id,
                    binding.account_id,
                    manifest.snapshot_id,
                    manifest.head_sequence,
                    binding.selected_car_id,
                    &transfer,
                    true,
                )?;
            }
            upsert_geofences_in_transaction(&transaction, vehicle_id, geofences)?;
            record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
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
            self.commit_catalogue_receipted_transaction(
                transaction,
                "import_generation_projection_state",
                vehicle_id,
                manifest.snapshot_id,
                StoreError::ImportGeneration,
            )
        })();
        finish_teslamate_projection_state_transfer(
            result,
            detach_teslamate_projection_state_transfer(self, &connection),
        )
    }

    /// Atomically publish a first schema-2.1 import and its prepared
    /// schema-2.2 successor. The immutable schema-2.2 no-op must be durable
    /// before this catalogue transaction; neither manifest is visible alone.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finalize_import_generation_with_projection_state_and_schema_22(
        &self,
        publication_gate: &PublicationGate,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: &ProjectionBinding,
        projection_state: &TeslaMateProjectionState,
        schema_22_manifest: &SyncManifest,
        schema_22_noop: &crate::updates_delivery::SignedNoOpState,
    ) -> Result<(), StoreError> {
        if run_id.is_nil()
            || source_id.is_nil()
            || vehicle_id.is_nil()
            || car_id <= 0
            || manifest.vehicle_id != vehicle_id
        {
            return Err(StoreError::InvalidImportGeneration);
        }
        validate_immutable_v2_base_binding(manifest, binding)?;
        if source_id != binding.account_id || car_id != binding.selected_car_id {
            return Err(StoreError::LineageCatalogConflict);
        }
        if schema_22_manifest.vehicle_id != vehicle_id
            || schema_22_manifest.installation_id != binding.installation_id
            || schema_22_manifest.account_id != binding.account_id
            || schema_22_manifest.generation != binding.generation
            || schema_22_manifest.head_sequence <= manifest.head_sequence
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        crate::updates_delivery::validate_schema_22_pair(schema_22_manifest, schema_22_noop)
            .map_err(|error| StoreError::InvalidSchema22Pair(error.message))?;
        let noop_bytes = serde_json::to_vec(schema_22_noop)
            .map_err(|error| StoreError::InvalidSchema22Pair(error.to_string()))?;
        self.prepare_schema_22_noop_publication(publication_gate, vehicle_id, None)?;
        self.publish_schema_22_noop(publication_gate, schema_22_noop)?;
        let stored_noop = self
            .schema_22_noop_for_snapshot(vehicle_id, schema_22_manifest.snapshot_id)?
            .ok_or(StoreError::Schema22NoOpNotFound)?;
        if stored_noop != noop_bytes {
            return Err(StoreError::InvalidSchema22Pair(
                "schema 2.2 no-op changed before catalogue publication".into(),
            ));
        }
        let transfer = projection_state
            .sealed_transfer_for_import_generation(run_id, binding.selected_car_id)?;
        let mut connection = self.open()?;
        attach_teslamate_projection_state_transfer(&connection, &transfer)?;
        let result = (|| -> Result<(), StoreError> {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(StoreError::Begin)?;
            let (encoded, base_last_observation_id, base_updated_at_ms): (String, i64, i64) =
                transaction
                    .query_row(
                        "SELECT sessions.session_json, generations.base_last_observation_id,
                            generations.base_updated_at_ms
                         FROM import_generation_sessions AS sessions
                         JOIN import_generations AS generations USING(run_id)
                         WHERE generations.run_id = ?1 AND generations.source_id = ?2
                           AND generations.vehicle_id = ?3 AND generations.car_id = ?4
                           AND generations.status = 'staging'",
                        params![
                            run_id.to_string(),
                            source_id.to_string(),
                            vehicle_id.to_string(),
                            car_id
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(StoreError::ImportGeneration)?
                    .ok_or(StoreError::ImportGenerationNotFound)?;
            let session =
                serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
            publish_manifest_in_transaction(&transaction, manifest, Some(binding))?;
            publish_manifest_in_transaction(&transaction, schema_22_manifest, None)?;
            promote_imported_open_session_in_transaction(
                &transaction,
                source_id,
                vehicle_id,
                car_id,
                &session,
                updated_at_ms,
                Some((base_last_observation_id, base_updated_at_ms)),
            )?;
            replace_teslamate_import_projection_state_from_attached_in_transaction(
                &transaction,
                vehicle_id,
                binding.account_id,
                manifest.snapshot_id,
                manifest.head_sequence,
                binding.selected_car_id,
                &transfer,
                true,
            )?;
            upsert_geofences_in_transaction(&transaction, vehicle_id, geofences)?;
            record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
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
            self.commit_catalogue_receipted_transaction(
                transaction,
                "import_generation_projection_state_schema_22",
                vehicle_id,
                schema_22_manifest.snapshot_id,
                StoreError::ImportGeneration,
            )
        })();
        finish_teslamate_projection_state_transfer(
            result,
            detach_teslamate_projection_state_transfer(self, &connection),
        )
    }

    fn finalize_import_generation_with_metadata(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: Option<&ProjectionBinding>,
    ) -> Result<(), StoreError> {
        if run_id.is_nil()
            || source_id.is_nil()
            || vehicle_id.is_nil()
            || car_id <= 0
            || manifest.vehicle_id != vehicle_id
        {
            return Err(StoreError::InvalidImportGeneration);
        }
        if manifest.schema == HUB_PROJECTION_SCHEMA_V2 && binding.is_none() {
            return Err(StoreError::ImmutableBaseBindingMissing(vehicle_id));
        }
        if let Some(binding) = binding {
            validate_immutable_v2_base_binding(manifest, binding)?;
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let (encoded, base_last_observation_id, base_updated_at_ms): (String, i64, i64) =
            transaction
                .query_row(
                    "SELECT sessions.session_json, generations.base_last_observation_id,
                        generations.base_updated_at_ms
                 FROM import_generation_sessions AS sessions
                 JOIN import_generations AS generations USING(run_id)
                 WHERE generations.run_id = ?1 AND generations.source_id = ?2
                   AND generations.vehicle_id = ?3 AND generations.car_id = ?4
                   AND generations.status = 'staging'",
                    params![
                        run_id.to_string(),
                        source_id.to_string(),
                        vehicle_id.to_string(),
                        car_id
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(StoreError::ImportGeneration)?
                .ok_or(StoreError::ImportGenerationNotFound)?;
        let session =
            serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
        publish_manifest_in_transaction(&transaction, manifest, binding)?;
        promote_imported_open_session_in_transaction(
            &transaction,
            source_id,
            vehicle_id,
            car_id,
            &session,
            updated_at_ms,
            Some((base_last_observation_id, base_updated_at_ms)),
        )?;
        upsert_geofences_in_transaction(&transaction, vehicle_id, geofences)?;
        record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
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
        self.commit_catalogue_receipted_transaction(
            transaction,
            "import_generation",
            vehicle_id,
            manifest.snapshot_id,
            StoreError::ImportGeneration,
        )
    }

    /// Atomically catalogue a sealed import history snapshot and its source
    /// fingerprint. Callers retain immutable pack chunks before this call.
    pub fn finalize_import_snapshot(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    ) -> Result<(), StoreError> {
        self.finalize_import_snapshot_with_metadata(
            manifest,
            fingerprint,
            geofences,
            None,
            None,
            None,
        )
    }

    /// As [`Self::finalize_import_snapshot`], but records the exact V2 base
    /// binding that was used to write the immutable pack.  All production V2
    /// publishers must use this entry point; a later delta must never infer
    /// account, generation, or selected car from mutable local state.
    pub fn finalize_import_snapshot_with_binding(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: &ProjectionBinding,
    ) -> Result<(), StoreError> {
        self.finalize_import_snapshot_with_metadata(
            manifest,
            fingerprint,
            geofences,
            Some(binding),
            None,
            None,
        )
    }

    /// Atomically catalogue a TeslaMate V2 base together with the exact
    /// source-owned projection inventory.  The inventory is what permits a
    /// future source rewrite to emit precise tombstones rather than silently
    /// retaining removed rows on a client.
    pub fn finalize_teslamate_import_snapshot(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: &ProjectionBinding,
        inventory: &TeslaMateImportProjectionInventory,
    ) -> Result<(), StoreError> {
        self.finalize_import_snapshot_with_metadata(
            manifest,
            fingerprint,
            geofences,
            Some(binding),
            Some(inventory),
            None,
        )
    }

    /// Atomically catalogue a TeslaMate V2 base with both legacy deletion
    /// inventory and the digest-only state needed by a later sparse successor.
    pub fn finalize_teslamate_import_snapshot_with_projection_state(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: &ProjectionBinding,
        inventory: &TeslaMateImportProjectionInventory,
        projection_state: &TeslaMateProjectionState,
    ) -> Result<(), StoreError> {
        self.finalize_import_snapshot_with_metadata(
            manifest,
            fingerprint,
            geofences,
            Some(binding),
            Some(inventory),
            Some(projection_state),
        )
    }

    fn finalize_import_snapshot_with_metadata(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
        binding: Option<&ProjectionBinding>,
        inventory: Option<&TeslaMateImportProjectionInventory>,
        projection_state: Option<&TeslaMateProjectionState>,
    ) -> Result<(), StoreError> {
        if manifest.schema == HUB_PROJECTION_SCHEMA_V2 && binding.is_none() {
            return Err(StoreError::ImmutableBaseBindingMissing(manifest.vehicle_id));
        }
        if let Some(binding) = binding {
            validate_immutable_v2_base_binding(manifest, binding)?;
        }
        if let Some(inventory) = inventory {
            let binding = binding.ok_or(StoreError::LineageCatalogConflict)?;
            validate_teslamate_import_inventory(manifest, binding, inventory)?;
        }
        if projection_state.is_some() && inventory.is_none() {
            return Err(StoreError::LineageCatalogConflict);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        publish_manifest_in_transaction(&transaction, manifest, binding)?;
        if let Some(inventory) = inventory {
            replace_teslamate_import_inventory_in_transaction(
                &transaction,
                manifest.vehicle_id,
                manifest.snapshot_id,
                manifest.head_sequence,
                inventory,
                true,
            )?;
        }
        if let Some(projection_state) = projection_state {
            let binding = binding.ok_or(StoreError::LineageCatalogConflict)?;
            replace_teslamate_import_projection_state_in_transaction(
                &transaction,
                manifest.vehicle_id,
                binding.account_id,
                manifest.snapshot_id,
                manifest.head_sequence,
                binding.selected_car_id,
                projection_state,
                true,
            )?;
        }
        upsert_geofences_in_transaction(&transaction, manifest.vehicle_id, geofences)?;
        record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
        self.commit_catalogue_receipted_transaction(
            transaction,
            "import_snapshot",
            manifest.vehicle_id,
            manifest.snapshot_id,
            StoreError::PublishManifest,
        )
    }

    /// Start an inactive import generation. Nothing in this generation is
    /// visible to lifecycle reads or published manifests.
    pub fn begin_import_generation(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        created_at_ms: i64,
    ) -> Result<Uuid, StoreError> {
        if source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        require_positive_db(car_id, "import car_id")?;
        validate_timestamp("import generation created_at_ms", created_at_ms)?;
        let run_id = Uuid::new_v4();
        let connection = self.open()?;
        let (base_last_observation_id, base_updated_at_ms): (i64, i64) = connection
            .query_row(
                "SELECT last_observation_id, updated_at_ms
                 FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::ImportGeneration)?
            .unwrap_or((0, 0));
        connection
            .execute(
                "INSERT INTO import_generations(
                    run_id, source_id, vehicle_id, car_id, status, created_at_ms,
                    base_last_observation_id, base_updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, 'staging', ?5, ?6, ?7)",
                params![
                    run_id.to_string(),
                    source_id.to_string(),
                    vehicle_id.to_string(),
                    car_id,
                    created_at_ms,
                    base_last_observation_id,
                    base_updated_at_ms
                ],
            )
            .map_err(StoreError::ImportGeneration)?;
        Ok(run_id)
    }

    /// Replace the inactive generation's open-session image. This is safe to
    /// call after each bounded source read; active lifecycle state is untouched.
    pub fn stage_import_generation_session(
        &self,
        run_id: Uuid,
        session: &TeslaMateOpenSession,
    ) -> Result<(), StoreError> {
        if run_id.is_nil() {
            return Err(StoreError::InvalidImportGeneration);
        }
        session
            .validate()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        let encoded = serde_json::to_string(session).map_err(StoreError::SerializeLifecycleRow)?;
        let connection = self.open()?;
        let updated = connection
            .execute(
                "INSERT INTO import_generation_sessions(run_id, session_json)
                 SELECT ?1, ?2 WHERE EXISTS(
                    SELECT 1 FROM import_generations
                    WHERE run_id = ?1 AND status = 'staging'
                 )
                 ON CONFLICT(run_id) DO UPDATE SET session_json = excluded.session_json",
                params![run_id.to_string(), encoded],
            )
            .map_err(StoreError::ImportGeneration)?;
        if updated == 0 {
            return Err(StoreError::ImportGenerationNotFound);
        }
        Ok(())
    }

    /// Atomically promote the already validated inactive session into the
    /// existing lifecycle tables and consume its staging generation. The
    /// caller invokes this after either pack publication or an unchanged
    /// completed-history fingerprint match.
    pub fn promote_import_generation(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
    ) -> Result<OpenSessionSeedReport, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let (encoded, base_last_observation_id, base_updated_at_ms): (String, i64, i64) =
            transaction
                .query_row(
                    "SELECT sessions.session_json, generations.base_last_observation_id,
                        generations.base_updated_at_ms
                 FROM import_generation_sessions AS sessions
                 JOIN import_generations AS generations USING(run_id)
                 WHERE run_id = ?1 AND source_id = ?2 AND vehicle_id = ?3
                   AND car_id = ?4 AND status = 'staging'",
                    params![
                        run_id.to_string(),
                        source_id.to_string(),
                        vehicle_id.to_string(),
                        car_id
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(StoreError::ImportGeneration)?
                .ok_or(StoreError::ImportGenerationNotFound)?;
        let session: TeslaMateOpenSession =
            serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
        let report = promote_imported_open_session_in_transaction(
            &transaction,
            source_id,
            vehicle_id,
            car_id,
            &session,
            updated_at_ms,
            Some((base_last_observation_id, base_updated_at_ms)),
        )?;
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
        transaction.commit().map_err(StoreError::ImportGeneration)?;
        Ok(report)
    }

    pub fn abort_import_generation(&self, run_id: Uuid) -> Result<(), StoreError> {
        if run_id.is_nil() {
            return Ok(());
        }
        let connection = self.open()?;
        connection
            .execute(
                "DELETE FROM import_generations WHERE run_id = ?1",
                params![run_id.to_string()],
            )
            .map_err(StoreError::ImportGeneration)?;
        Ok(())
    }
}
