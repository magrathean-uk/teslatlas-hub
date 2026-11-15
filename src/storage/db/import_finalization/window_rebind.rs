// SPDX-License-Identifier: AGPL-3.0-only

// The first import onto a window-only Hub (DEC-150) publishes its base and
// the seam on it in one catalogue transaction (DEC-151 follow-up, M03-T24).
// The seam is planned against the uncommitted rebind in a transaction that
// is rolled back; publication applies the rebind again, proves the same seam
// and commits both, so an interruption leaves either the old window lineage
// or the complete new one.

/// The base a window rebind publishes in place of the collector's lineage
/// `expected`, and the source sessions its seam is planned from.
pub(crate) struct WindowRebindBase<'a> {
    pub(crate) expected: &'a LineageManifestV2,
    pub(crate) run_id: Uuid,
    pub(crate) source_id: Uuid,
    pub(crate) vehicle_id: Uuid,
    pub(crate) car_id: i64,
    pub(crate) updated_at_ms: i64,
    pub(crate) manifest: &'a SyncManifest,
    pub(crate) geofences: &'a [crate::source_db_projection::SourceDbGeofence],
    pub(crate) binding: &'a ProjectionBinding,
    pub(crate) car: &'a ProjectionCar,
    pub(crate) drives: &'a [ProjectionDrive],
    pub(crate) charges: &'a [crate::hub_pack::ProjectionCharge],
}

impl<'a> WindowRebindBase<'a> {
    fn publication(&self) -> ImportBasePublication<'a> {
        ImportBasePublication {
            run_id: self.run_id,
            source_id: self.source_id,
            vehicle_id: self.vehicle_id,
            car_id: self.car_id,
            updated_at_ms: self.updated_at_ms,
            manifest: self.manifest,
            geofences: self.geofences,
            binding: self.binding,
            materialisation: Some((self.car, self.drives)),
            retain_legacy_inventory: false,
            replacement: Some(BaseReplacement::Window(self.expected)),
        }
    }
}

/// The seam planned on an uncommitted window rebind: the seam the packs
/// carry with the decision it rests on, the Hub's sync it publishes as live
/// deltas, and the rebound base's head and pack slots.
#[derive(Debug, Clone)]
pub(crate) struct WindowRebindSeamStage {
    pub(crate) decided: ImportSeamDryRun,
    pub(crate) sync: Vec<StagedSeamSync>,
    pub(crate) head_sequence: u64,
    pub(crate) head_digest: Sha256Digest,
    pub(crate) base_pack_count: usize,
    pub(crate) next_ordinal: u32,
}

/// The seam's delta packs a window rebind publishes with its base.
pub(crate) struct WindowRebindSeam<'a> {
    pub(crate) deltas: &'a [LineageDelta],
    pub(crate) cursor_key: &'a CursorKey,
    pub(crate) terminal_cursor: &'a OpaqueCursor,
    pub(crate) fingerprint: Sha256Digest,
    /// The final projection state, the seam's forms held.
    pub(crate) projection_state: &'a SourceDbProjectionState,
    pub(crate) seam_digest: [u8; 32],
    pub(crate) staged: &'a [StagedSeamSync],
}

/// The steps of a window rebind at which tests interrupt it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowRebindStep {
    /// The seam is planned; the rehearsal transaction is rolled back.
    SeamStaged,
    /// The seam's delta packs are written; nothing is catalogued.
    SeamPacksWritten,
    /// The publication transaction holds the rebound base, not the seam.
    BaseInTransaction,
    /// The publication transaction holds both and has not committed.
    SeamInTransaction,
}

#[cfg(test)]
thread_local! {
    static WINDOW_REBIND_INTERRUPTION: std::cell::Cell<Option<WindowRebindStep>> =
        const { std::cell::Cell::new(None) };
}

/// Interrupts this thread's next window rebind at `step` (tests only).
#[cfg(test)]
pub(crate) fn interrupt_window_rebind_at(step: WindowRebindStep) {
    WINDOW_REBIND_INTERRUPTION.with(|armed| armed.set(Some(step)));
}

/// Fails when a test interrupts the window rebind at `step`.
pub(crate) fn window_rebind_step(step: WindowRebindStep) -> Result<(), StoreError> {
    #[cfg(test)]
    if WINDOW_REBIND_INTERRUPTION.with(|armed| armed.get() == Some(step)) {
        WINDOW_REBIND_INTERRUPTION.with(|armed| armed.set(None));
        return Err(StoreError::CatalogueDurability(std::io::Error::other(format!(
            "window rebind interrupted: {step:?}"
        ))));
    }
    #[cfg(not(test))]
    let _ = step;
    Ok(())
}

impl HubStore {
    /// Whether the vehicle's V2 base is one the collector made before any
    /// import (DEC-150): bound to the `owner_api_compat` source, with no
    /// import projection state, no legacy import inventory and no recorded
    /// seam decision. The first import may rebind such a base once.
    pub(crate) fn window_only_v2_base_is_rebindable(
        &self,
        vehicle_id: Uuid,
    ) -> Result<bool, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        window_only_v2_base_in_connection(&connection, &vehicle_id.to_string())
    }

    /// The checks of a window rebind made before its transactions: the
    /// vehicle's lineage is still `expected`, and the new base is bound to
    /// the import and follows it.
    fn checked_window_rebind<'a>(
        &self,
        base: &WindowRebindBase<'a>,
    ) -> Result<ImportBasePublication<'a>, StoreError> {
        let expected = base.expected;
        let current = self
            .lineage_manifest_for_vehicle(base.vehicle_id)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        if serde_json::to_vec(&current).map_err(StoreError::SerializeManifest)?
            != serde_json::to_vec(expected).map_err(StoreError::SerializeManifest)?
            || expected.schema != HUB_PROJECTION_SCHEMA_V2
            || expected.installation_id != base.binding.installation_id
            || expected.vehicle_id != base.binding.vehicle_id
            || base.manifest.snapshot_id == expected.base.snapshot_id
            || base.manifest.head_sequence <= expected.head_sequence
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let publication = base.publication();
        publication.validate()?;
        Ok(publication)
    }

    /// Plans the seam of a window rebind against the rebind itself, applied
    /// with its base projection state `base_state` in a transaction that is
    /// then rolled back. Nothing in the store changes.
    pub(crate) fn stage_window_rebind_seam(
        &self,
        _gate: &PublicationGate,
        base: &WindowRebindBase<'_>,
        base_state: &SourceDbProjectionState,
    ) -> Result<WindowRebindSeamStage, StoreError> {
        let publication = self.checked_window_rebind(base)?;
        let transfer =
            base_state.sealed_transfer_for_import_generation(base.run_id, base.binding.selected_car_id)?;
        let mut connection = self.open()?;
        attach_source_db_projection_state_transfer(&connection, &transfer)?;
        let result = (|| -> Result<WindowRebindSeamStage, StoreError> {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(StoreError::Begin)?;
            publish_import_base_in_transaction(&transaction, &publication, &transfer)?;
            let stage = stage_rebound_seam_in_transaction(&transaction, base)?;
            transaction.rollback().map_err(StoreError::LifecycleWrite)?;
            Ok(stage)
        })();
        finish_source_db_projection_state_transfer(
            result,
            detach_source_db_projection_state_transfer(self, &connection),
        )
    }

    /// Publishes a window rebind and its seam in one catalogue transaction:
    /// the rebind is applied again, the seam's publication proves the plan
    /// and sync it was staged with (or refuses with
    /// [`StoreError::ImportGenerationConflict`]), and the generation ends.
    pub(crate) fn finalize_window_rebind_with_staged_seam(
        &self,
        _gate: &PublicationGate,
        base: &WindowRebindBase<'_>,
        seam: WindowRebindSeam<'_>,
    ) -> Result<(), StoreError> {
        let publication = self.checked_window_rebind(base)?;
        if seam.deltas.len() <= seam.staged.len() {
            return Err(StoreError::LineageCatalogConflict);
        }
        let successors = DeltaSuccessorPublication {
            run_id: base.run_id,
            source_id: base.source_id,
            vehicle_id: base.vehicle_id,
            car_id: base.car_id,
            updated_at_ms: base.updated_at_ms,
            deltas: seam.deltas,
            terminal_cursor: seam.terminal_cursor,
            fingerprint: seam.fingerprint,
            geofences: base.geofences,
            materialisation: Some((base.car, base.drives)),
            retain_legacy_inventory: false,
            seam: Some(ImportSeamPublication {
                digest: seam.seam_digest,
                charges: base.charges,
                staged: Some(seam.staged),
            }),
        };
        let final_delta = successors.final_delta()?;
        self.verify_delta_successors(&successors, base.binding, seam.cursor_key)?;
        let transfer = seam
            .projection_state
            .sealed_transfer_for_import_generation(base.run_id, base.binding.selected_car_id)?;
        let mut connection = self.open()?;
        attach_source_db_projection_state_transfer(&connection, &transfer)?;
        let result = (|| -> Result<(), StoreError> {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(StoreError::Begin)?;
            publish_import_base_in_transaction(&transaction, &publication, &transfer)?;
            window_rebind_step(WindowRebindStep::BaseInTransaction)?;
            successors.validate_lineage(rebound_lineage_in_transaction(&transaction, base)?)?;
            publish_delta_successors_in_transaction(
                &transaction,
                &successors,
                base.binding,
                SuccessorProjectionState::AdvanceHead,
            )?;
            window_rebind_step(WindowRebindStep::SeamInTransaction)?;
            self.commit_catalogue_receipted_transaction(
                transaction,
                "import_generation_window_rebind",
                base.vehicle_id,
                final_delta.pack.snapshot_id,
                StoreError::ImportGeneration,
            )
        })();
        finish_source_db_projection_state_transfer(
            result,
            detach_source_db_projection_state_transfer(self, &connection),
        )
    }
}

/// The seam on the rebound base `transaction` holds, worked out as
/// [`HubStore::stage_import_seam_ahead_of_packs`] does on a committed base:
/// the seam's Hub side is applied in the transaction (the caller rolls it
/// back) and every sync mutation then waiting is staged.
fn stage_rebound_seam_in_transaction(
    transaction: &Transaction<'_>,
    base: &WindowRebindBase<'_>,
) -> Result<WindowRebindSeamStage, StoreError> {
    let (vehicle_id, drives, charges) = (base.vehicle_id, base.drives, base.charges);
    let mut previous_id = 0_i64;
    for drive in drives {
        if drive.id <= previous_id
            || drive.id >= crate::db::hub_ids::HUB_ID_BASE
            || drive.car_id != base.car_id
        {
            return Err(StoreError::InvalidImportGeneration);
        }
        previous_id = drive.id;
    }
    validate_import_seam_charges(charges, base.car_id)?;
    let vehicle_key = vehicle_id.to_string();
    let seam_history = import_seam_history_in_connection(transaction, &vehicle_key)?;
    let prior = durable_prior_imported_drive_ids_in_transaction(transaction, &vehicle_key)?;
    let (coverage, before) =
        import_seam_plan_in_transaction(transaction, vehicle_id, drives, charges, &prior)?;
    let changed_the_store = before.changes_the_store();
    let packs = crate::import::source_db::seam::SeamPackPlan::for_plan(&coverage, &before);
    if changed_the_store {
        apply_import_seam_hub_side_in_transaction(transaction, vehicle_id, &before)?;
        apply_import_seam_to_charges_in_transaction(transaction, vehicle_id, &before, charges, false)?;
        // Planned again, the applied plan must give the packs the same
        // decisions, or publication could never prove them.
        let (coverage_after, after) =
            import_seam_plan_in_transaction(transaction, vehicle_id, drives, charges, &prior)?;
        if crate::import::source_db::seam::SeamPackPlan::for_plan(&coverage_after, &after).digest()
            != packs.digest()
        {
            return Err(StoreError::ImportGenerationConflict);
        }
    }
    let restated_drives = restated_import_drives_in_transaction(transaction, &vehicle_key, drives, &packs)?;
    let restated_charges = restated_import_charges(&before, &packs);
    let unpublished: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1 AND published = 0",
            params![vehicle_key.as_str()],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    let decided = ImportSeamDryRun {
        ahead: ImportSeamAhead {
            packs,
            restated_drives,
            restated_charges,
            changed_the_store,
        },
        seam_history,
        unpublished: u64::try_from(unpublished).map_err(|_| StoreError::InvalidStoredSequence)?,
    };
    let sync = staged_seam_sync_in_transaction(transaction, vehicle_id, base.binding)?;
    let (head_sequence, head_digest): (i64, String) = transaction
        .query_row(
            "SELECT head_sequence, head_digest FROM sync_heads
              WHERE vehicle_id = ?1 AND base_snapshot_id = ?2",
            params![vehicle_key.as_str(), base.manifest.snapshot_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?
        .ok_or(StoreError::LineageCatalogConflict)?;
    let maximum_ordinal: Option<i64> = transaction
        .query_row(
            "SELECT MAX(ordinal) FROM sync_packs WHERE snapshot_id = ?1",
            params![base.manifest.snapshot_id.to_string()],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    Ok(WindowRebindSeamStage {
        decided,
        sync,
        head_sequence: u64::try_from(head_sequence).map_err(|_| StoreError::InvalidStoredSequence)?,
        head_digest: head_digest
            .parse::<Sha256Digest>()
            .map_err(|_| StoreError::LineageCatalogConflict)?,
        base_pack_count: base.manifest.chunks.len(),
        next_ordinal: maximum_ordinal
            .unwrap_or(-1)
            .checked_add(1)
            .and_then(|ordinal| u32::try_from(ordinal).ok())
            .ok_or(StoreError::LineageCatalogConflict)?,
    })
}

/// The lineage of the base `transaction` just published for a window
/// rebind: its packs and head, with no delta yet.
fn rebound_lineage_in_transaction(
    transaction: &Transaction<'_>,
    base: &WindowRebindBase<'_>,
) -> Result<LineageManifestV2, StoreError> {
    let vehicle_key = base.vehicle_id.to_string();
    let (snapshot_id, base_sequence, base_digest, packs_json): (String, i64, String, Vec<u8>) = transaction
        .query_row(
            "SELECT snapshot_id, base_sequence, base_digest, packs_json
               FROM sync_bases WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?
        .ok_or(StoreError::LineageCatalogConflict)?;
    let (head_sequence, head_digest, terminal_cursor): (i64, String, String) = transaction
        .query_row(
            "SELECT head_sequence, head_digest, terminal_cursor FROM sync_heads
              WHERE vehicle_id = ?1 AND base_snapshot_id = ?2",
            params![vehicle_key.as_str(), snapshot_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?
        .ok_or(StoreError::LineageCatalogConflict)?;
    let deltas: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM sync_deltas WHERE vehicle_id = ?1",
            params![vehicle_key.as_str()],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if snapshot_id != base.manifest.snapshot_id.to_string() || deltas != 0 {
        return Err(StoreError::LineageCatalogConflict);
    }
    let packs: Vec<TransportPack> =
        serde_json::from_slice(&packs_json).map_err(StoreError::DeserializeManifest)?;
    let schema = packs.first().ok_or(StoreError::LineageCatalogConflict)?.schema;
    let lineage = LineageManifestV2 {
        protocol: LINEAGE_PROTOCOL_V2,
        capability: LineageCapability::ImmutableBaseOrderedDeltas,
        schema,
        installation_id: base.binding.installation_id,
        account_id: base.binding.account_id,
        vehicle_id: base.vehicle_id,
        generation: base.binding.generation,
        base: LineageBase {
            snapshot_id: base.manifest.snapshot_id,
            sequence: u64::try_from(base_sequence).map_err(|_| StoreError::InvalidStoredSequence)?,
            digest: base_digest.parse::<Sha256Digest>().map_err(|_| StoreError::LineageCatalogConflict)?,
            packs,
        },
        deltas: Vec::new(),
        head_sequence: u64::try_from(head_sequence).map_err(|_| StoreError::InvalidStoredSequence)?,
        head_digest: head_digest.parse::<Sha256Digest>().map_err(|_| StoreError::LineageCatalogConflict)?,
        terminal_cursor: serde_json::from_str(&terminal_cursor).map_err(StoreError::DeserializeManifest)?,
    };
    lineage.validate().map_err(StoreError::Manifest)?;
    Ok(lineage)
}
