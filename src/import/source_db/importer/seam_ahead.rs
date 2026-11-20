// SPDX-License-Identifier: AGPL-3.0-only

/// The seam plan applied to one successor's delta rows (design M03-SEAM-R3,
/// section 4): the packs carry the planned source rows only. A skipped drive
/// or charge is left out, a packed one carries the bounds the plan gives it,
/// a source position or charge sample inside coverage is left out and one
/// whose session was skipped is packed under the session that stands for
/// it. Packed drives whose stored row is not yet the planned row are
/// restated even when the source did not change them, so the packs and the
/// store hold the same drives; packed charges the plan widened or readmitted
/// are restated the same way.
#[derive(Debug, Clone)]
struct DeltaSeam {
    packs: crate::import::source_db::seam::SeamPackPlan,
    restated: Vec<ProjectionDrive>,
    emitted_drives: HashSet<i64>,
    restated_charges: Vec<ProjectionCharge>,
    emitted_charges: HashSet<i64>,
    skipped: SourceDbSeamSkipped,
    /// Rows an earlier import published that the seam now leaves out
    /// (M03-T19): the packs tombstone them.
    withdrawn: Vec<ProjectionTombstone>,
}

impl DeltaSeam {
    fn new(
        ahead: &crate::db::ImportSeamAhead,
        drives: &[ProjectionDrive],
        charges: &[ProjectionCharge],
    ) -> Self {
        use crate::import::source_db::seam::PackedSession;
        let restated_ids: HashSet<i64> = ahead.restated_drives.iter().copied().collect();
        let restated = drives
            .iter()
            .filter(|drive| restated_ids.contains(&drive.id))
            .filter_map(|drive| match ahead.packs.drive(drive.id) {
                Some(PackedSession::Keep { bounds }) => {
                    Some(crate::db::planned_import_drive(drive, bounds))
                }
                _ => None,
            })
            .collect();
        let restated_charge_ids: HashSet<i64> = ahead.restated_charges.iter().copied().collect();
        let restated_charges = charges
            .iter()
            .filter(|charge| restated_charge_ids.contains(&charge.id))
            .filter_map(|charge| match ahead.packs.charge(charge.id) {
                Some(PackedSession::Keep { bounds }) => {
                    Some(crate::db::planned_import_charge(charge, bounds))
                }
                _ => None,
            })
            .collect();
        Self {
            packs: ahead.packs.clone(),
            restated,
            emitted_drives: HashSet::new(),
            restated_charges,
            emitted_charges: HashSet::new(),
            skipped: SourceDbSeamSkipped::default(),
            withdrawn: Vec::new(),
        }
    }

    /// Whether a source tombstone would remove a session the store keeps on
    /// the Hub's side (M03-T19): the packs never carry it.
    fn retains(&self, tombstone: &ProjectionTombstone) -> bool {
        match tombstone.entity {
            ProjectionDeltaEntity::Drive => self.packs.retains_drive(tombstone.id),
            ProjectionDeltaEntity::Charge => self.packs.retains_charge(tombstone.id),
            _ => false,
        }
    }

    /// Applies the plan to one changed source charge; false when the packs
    /// leave it out (counted as seam-skipped).
    fn pack_charge(&mut self, charge: &mut ProjectionCharge) -> Result<bool, SourceDbImportError> {
        use crate::import::source_db::seam::PackedSession;
        match self.packs.charge(charge.id) {
            None => Err(SourceDbImportError::InvalidDirectDeltaRow),
            Some(PackedSession::Skip) => {
                self.skipped.charges = self.skipped.charges.saturating_add(1);
                Ok(false)
            }
            Some(PackedSession::Keep { bounds }) => {
                *charge = crate::db::planned_import_charge(charge, bounds);
                self.emitted_charges.insert(charge.id);
                Ok(true)
            }
        }
    }

    /// Applies the instant rule to one changed source charge sample; false
    /// when the packs leave it out (counted as seam-skipped).
    fn pack_charge_sample(&mut self, sample: &mut ProjectionChargeSample) -> bool {
        use crate::import::source_db::seam::{ChildRow, PackedChild};
        let child = ChildRow {
            id: sample.id,
            at_ms: sample.timestamp_ms,
            parent: Some(sample.charge_process_id),
        };
        match self.packs.charge_sample(child) {
            PackedChild::Skip => {
                self.skipped.charge_samples = self.skipped.charge_samples.saturating_add(1);
                false
            }
            PackedChild::Keep { parent } => {
                sample.charge_process_id = parent.unwrap_or(sample.charge_process_id);
                true
            }
        }
    }

    /// Appends the packed sessions the source did not change but the plan
    /// restates: drives the store does not yet hold as planned, and charges
    /// the plan widened or readmitted.
    fn append_restated<F>(
        &self,
        current: &mut DirectDeltaRows,
        emitted_batches: &mut usize,
        maximum_batches: usize,
        binding: &ProjectionBinding,
        visit: &mut F,
    ) -> Result<(), SourceDbImportError>
    where
        F: FnMut(DirectDeltaRows) -> Result<(), SourceDbImportError>,
    {
        for drive in &self.restated {
            if self.emitted_drives.contains(&drive.id) {
                continue;
            }
            let bytes = serde_json::to_vec(drive).map_err(SourceDbImportError::SerializeDirectDeltaRow)?;
            make_room_for_restated_row(
                current,
                emitted_batches,
                maximum_batches,
                visit,
                bytes.len(),
                drive.car_id != binding.selected_car_id || drive.id <= 0,
            )?;
            current.drives.push(drive.clone());
        }
        for charge in &self.restated_charges {
            if self.emitted_charges.contains(&charge.id) {
                continue;
            }
            let bytes =
                serde_json::to_vec(charge).map_err(SourceDbImportError::SerializeDirectDeltaRow)?;
            make_room_for_restated_row(
                current,
                emitted_batches,
                maximum_batches,
                visit,
                bytes.len(),
                charge.car_id != binding.selected_car_id || charge.id <= 0,
            )?;
            current.charges.push(charge.clone());
        }
        Ok(())
    }
}

/// Reserves room in `current` for one restated row of `bytes`, emitting the
/// batch first when the row does not fit.
fn make_room_for_restated_row<F>(
    current: &mut DirectDeltaRows,
    emitted_batches: &mut usize,
    maximum_batches: usize,
    visit: &mut F,
    bytes: usize,
    invalid: bool,
) -> Result<(), SourceDbImportError>
where
    F: FnMut(DirectDeltaRows) -> Result<(), SourceDbImportError>,
{
    let payload_bytes =
        u64::try_from(bytes).map_err(|_| SourceDbImportError::DirectDeltaBatchOverflow)?;
    if payload_bytes > DIRECT_DELTA_BATCH_PAYLOAD_BYTES {
        return Err(SourceDbImportError::DirectDeltaRowTooLarge { payload_bytes });
    }
    if current.next_would_exceed(payload_bytes)? && !current.is_empty() {
        emit_direct_delta_batch(emitted_batches, maximum_batches, std::mem::take(current), visit)?;
    }
    if invalid {
        return Err(SourceDbImportError::InvalidDirectDeltaRow);
    }
    current.reserve(payload_bytes)
}

/// A direct successor with its seam staged ahead of its packs.
struct PlannedDirectSuccessor {
    seam: DeltaSeam,
    seam_digest: [u8; 32],
    /// The sync the seam publishes with the packs, as live deltas ahead of
    /// them (M03-T19).
    seam_sync: Vec<crate::db::StagedSeamSync>,
    from_sequence: u64,
    parent_digest: Sha256Digest,
    remaining_delta_packs: usize,
}

/// How a direct successor goes on once its pack capacity is known. One value
/// exists per import, so the variants' size difference does not matter.
#[allow(clippy::large_enum_variant)]
enum DirectSuccessorRoute {
    /// Typed deltas on the current base; the seam is already applied.
    Deltas(PlannedDirectSuccessor),
    /// A new base: the deltas no longer fit and the vehicle has no seam
    /// history. The seam is not applied: the full snapshot carries the
    /// source rows, and its publication refuses, in the transaction that
    /// would retire the lineage, a vehicle that gained seam history since.
    NewBase(ReplacedLineage),
}

/// The lineage a new base replaces.
struct ReplacedLineage {
    existing: crate::protocol::LineageManifestV2,
    existing_pack_count: usize,
    head_sequence: u64,
}

/// How many times a direct successor is decided again when the store
/// changed between its decision and the seam (a window opened or the Hub
/// collected meanwhile) before the import gives up, having changed nothing.
const SUCCESSOR_DECISION_ATTEMPTS: usize = 3;

/// The importer's successor step (DEC-103): routes the successor and, when
/// it is refused, discards the full-snapshot candidate packs and aborts the
/// run before the refusal is returned, so the store, the packs and the
/// published head stay as they were.
#[allow(clippy::too_many_arguments)]
fn route_direct_successor_for_run(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    selected_car_id: i64,
    drives: &[ProjectionDrive],
    charges: &[ProjectionCharge],
    capture: &mut SourceDbProjectionStateCapture,
    binding: &ProjectionBinding,
    selected_car: &ProjectionCar,
    max_chunks: usize,
    direct: &mut StagedProjectionPacks,
    run_guard: &mut ImportGenerationGuard<'_>,
    before_seam: impl FnMut(&HubStore) -> Result<(), SourceDbImportError>,
) -> Result<DirectSuccessorRoute, SourceDbImportError> {
    match route_direct_successor(
        store,
        publication_gate,
        cursor_key,
        vehicle_id,
        selected_car_id,
        drives,
        charges,
        capture,
        binding,
        selected_car,
        max_chunks,
        before_seam,
    ) {
        Err(refused @ SourceDbImportError::ImportBaseReplacementRefused { .. }) => {
            // The full-snapshot candidate is no base: remove it so the
            // packs stay as they were, and abort the run.
            direct.keep_chunks();
            discard_unpublished_chunks(store, publication_gate, &direct.chunks)?;
            if let Some(run_id) = run_guard.run_id.take() {
                store.abort_import_generation(run_id)?;
            }
            Err(refused)
        }
        other => other,
    }
}

/// Decides how a direct successor goes on (DEC-103). Once the Hub has
/// collected the vehicle its import base is never replaced: the Hub's rows
/// reach clients only as deltas of that base's lineage, so a new base would
/// drop them. A successor whose changes no longer fit in the lineage's packs
/// is then refused before the seam changes the store, so the store, the
/// packs and the published head stay as they were.
///
/// The decision and the seam are one step. A dry run of the seam gives the
/// vehicle's seam history, the plan and an upper bound on the deltas that
/// publishing the waiting sync mutations adds (one per claim); the seam is
/// then applied only if the transaction that applies it finds the same,
/// and otherwise rolled back and decided again. So the seam history is the
/// vehicle's before this import's own seam, a window opened or a Hub row
/// collected after the dry run is seen, and no refusal follows a change.
/// `before_seam` runs between the decision and the seam (tests use it to
/// collect meanwhile).
#[allow(clippy::too_many_arguments)]
fn route_direct_successor(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    selected_car_id: i64,
    drives: &[ProjectionDrive],
    charges: &[ProjectionCharge],
    capture: &mut SourceDbProjectionStateCapture,
    binding: &ProjectionBinding,
    selected_car: &ProjectionCar,
    max_chunks: usize,
    mut before_seam: impl FnMut(&HubStore) -> Result<(), SourceDbImportError>,
) -> Result<DirectSuccessorRoute, SourceDbImportError> {
    for _ in 0..SUCCESSOR_DECISION_ATTEMPTS {
        let decided = store.dry_run_import_seam_ahead_of_packs(
            publication_gate,
            vehicle_id,
            selected_car_id,
            drives,
            charges,
        )?;
        // The lineage changes only under the publication gate the importer
        // holds, so it is the same when the seam is applied.
        let existing = store
            .lineage_manifest_for_vehicle(vehicle_id)?
            .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
        let used = existing
            .base
            .packs
            .len()
            .checked_add(existing.deltas.len())
            .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
        let live_deltas = usize::try_from(decided.unpublished.div_ceil(SEAM_MUTATION_CLAIM_LIMIT as u64))
            .map_err(|_| SourceDbImportError::DirectDeltaBatchOverflow)?;
        let remaining = max_chunks.saturating_sub(used.saturating_add(live_deltas));
        let exhausted = direct_delta_capacity_exhausted_with_seam(
            capture,
            binding,
            selected_car,
            remaining,
            Some(&DeltaSeam::new(&decided.ahead, drives, charges)),
        )?;
        if exhausted && decided.seam_history {
            return Err(SourceDbImportError::ImportBaseReplacementRefused {
                used,
                limit: max_chunks,
            });
        }
        before_seam(store)?;
        if exhausted {
            // A new base applies no seam; it stands only if the store still
            // gives the same decision.
            if store.dry_run_import_seam_ahead_of_packs(
                publication_gate,
                vehicle_id,
                selected_car_id,
                drives,
                charges,
            )? != decided
            {
                continue;
            }
            let (_, head_sequence, _) = store
                .v2_head(vehicle_id)?
                .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
            return Ok(DirectSuccessorRoute::NewBase(ReplacedLineage {
                existing,
                existing_pack_count: used,
                head_sequence: u64::try_from(head_sequence)
                    .map_err(|_| crate::db::StoreError::InvalidStoredSequence)?,
            }));
        }
        let Some(planned) = plan_direct_successor_with_limit(
            store,
            publication_gate,
            cursor_key,
            vehicle_id,
            selected_car_id,
            drives,
            charges,
            max_chunks,
            Some(&decided),
        )?
        else {
            continue;
        };
        // The deltas fit within the bound, so they fit the lineage the
        // seam's publication left.
        return Ok(DirectSuccessorRoute::Deltas(planned));
    }
    // The store kept changing between decision and seam; nothing was applied.
    Err(crate::db::StoreError::ImportGenerationConflict.into())
}

#[cfg(test)]
/// Stages the import seam ahead of a direct successor's packs (design
/// M03-SEAM-R3, section 4; M03-T19): nothing is applied or published yet.
/// Publication applies the seam and makes its sync visible together with the
/// packs that carry the replacements, under one head, so every head a client
/// can read holds each session once and a failed import leaves the previous
/// one in sync.
fn plan_direct_successor(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    selected_car_id: i64,
    drives: &[ProjectionDrive],
    charges: &[ProjectionCharge],
) -> Result<PlannedDirectSuccessor, SourceDbImportError> {
    plan_direct_successor_with_limit(
        store,
        publication_gate,
        cursor_key,
        vehicle_id,
        selected_car_id,
        drives,
        charges,
        ProtocolLimits::default().max_chunks,
        None,
    )?
    .ok_or_else(|| crate::db::StoreError::ImportGenerationConflict.into())
}

/// As [`plan_direct_successor`], against a lineage of at most `max_chunks`
/// packs. With `decided`, the seam is staged only on the store the decision
/// saw; `None` when it changed.
#[allow(clippy::too_many_arguments)]
fn plan_direct_successor_with_limit(
    store: &HubStore,
    publication_gate: &PublicationGate,
    _cursor_key: &CursorKey,
    vehicle_id: Uuid,
    selected_car_id: i64,
    drives: &[ProjectionDrive],
    charges: &[ProjectionCharge],
    max_chunks: usize,
    decided: Option<&crate::db::ImportSeamDryRun>,
) -> Result<Option<PlannedDirectSuccessor>, SourceDbImportError> {
    let Some(staged) = store.stage_import_seam_ahead_of_packs(
        publication_gate,
        vehicle_id,
        selected_car_id,
        drives,
        charges,
        decided,
    )?
    else {
        return Ok(None);
    };
    let (_, head_sequence, head_digest) = store
        .v2_head(vehicle_id)?
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    let from_sequence =
        u64::try_from(head_sequence).map_err(|_| crate::db::StoreError::InvalidStoredSequence)?;
    let existing = store
        .lineage_manifest_for_vehicle(vehicle_id)?
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    let existing_pack_count = existing
        .base
        .packs
        .len()
        .checked_add(existing.deltas.len())
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    let remaining_delta_packs = max_chunks
        .checked_sub(existing_pack_count)
        .and_then(|remaining| remaining.checked_sub(staged.sync.len()))
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    Ok(Some(PlannedDirectSuccessor {
        seam: DeltaSeam::new(&staged.ahead, drives, charges),
        seam_digest: staged.ahead.packs.digest(),
        seam_sync: staged.sync,
        from_sequence,
        parent_digest: head_digest,
        remaining_delta_packs,
    }))
}

/// The most sync mutations one publication turn claims, and so publishes
/// as one delta.
const SEAM_MUTATION_CLAIM_LIMIT: usize = 10_000;

/// Publishes every unpublished sync mutation of the vehicle as live deltas
/// under the publication gate the importer holds (the tests' collector).
#[cfg(test)]
fn publish_sync_mutations_ahead_of_packs(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
) -> Result<(), SourceDbImportError> {
    loop {
        if !store.has_unpublished_sync_mutations(vehicle_id)? {
            return Ok(());
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|value| i64::try_from(value.as_millis()).ok())
            .ok_or_else(|| SourceDbImportError::SeamPublication("the clock is unusable".to_owned()))?;
        let turn = crate::collection::collector::publish_unpublished_sync_mutations(
            store,
            cursor_key,
            vehicle_id,
            now_ms,
            publication_gate,
            SEAM_MUTATION_CLAIM_LIMIT,
        )
        .map_err(|error| match error {
            crate::collection::collector::CollectorError::Store(error) => error.into(),
            other => SourceDbImportError::SeamPublication(other.to_string()),
        })?;
        if turn.published == 0 {
            // Mutations remain but none could be claimed (another claim
            // holds them): the packs must not go ahead of them.
            return Err(SourceDbImportError::SeamPublication(
                "unpublished sync mutations are claimed elsewhere".to_owned(),
            ));
        }
    }
}

/// The run a planned direct successor belongs to.
struct PlannedSuccessorRun<'a> {
    run_id: Uuid,
    source_id: Uuid,
    imported_at_ms: i64,
    base_snapshot_id: Uuid,
    minimum_free_bytes: u64,
    fingerprint: Sha256Digest,
    geofences: &'a [crate::source_db_projection::SourceDbGeofence],
    materialised_car: &'a ProjectionCar,
    materialised_drives: &'a [ProjectionDrive],
    materialised_charges: &'a [ProjectionCharge],
}

/// Where a planned direct successor's packs are catalogued.
#[derive(Clone, Copy)]
enum SuccessorTarget<'a, 'b> {
    /// On the vehicle's committed head.
    Head,
    /// With the window rebind that publishes their base, in one catalogue
    /// transaction (M03-T24); the packs take the ordinals from `next_ordinal`.
    WindowRebind {
        base: &'a crate::db::WindowRebindBase<'b>,
        next_ordinal: u32,
    },
}

/// What a planned direct successor published.
#[derive(Debug)]
struct PublishedDirectSuccessor {
    sequence: u64,
    projected_rows: u64,
}

/// Writes a planned direct successor's delta packs (the planned source rows
/// only) and publishes them on the vehicle's head; publication plans the
/// seam again and refuses a different plan.
#[allow(clippy::too_many_arguments)]
fn publish_planned_direct_successor(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    run: PlannedSuccessorRun<'_>,
    binding: &ProjectionBinding,
    selected_car: &ProjectionCar,
    capture: SourceDbProjectionStateCapture,
    planned: &mut PlannedDirectSuccessor,
) -> Result<PublishedDirectSuccessor, SourceDbImportError> {
    publish_planned_direct_successor_to(
        store,
        publication_gate,
        cursor_key,
        run,
        binding,
        selected_car,
        capture,
        planned,
        SuccessorTarget::Head,
    )
}

/// As [`publish_planned_direct_successor`], catalogued at `target`.
#[allow(clippy::too_many_arguments)]
fn publish_planned_direct_successor_to(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    run: PlannedSuccessorRun<'_>,
    binding: &ProjectionBinding,
    selected_car: &ProjectionCar,
    mut capture: SourceDbProjectionStateCapture,
    planned: &mut PlannedDirectSuccessor,
    target: SuccessorTarget<'_, '_>,
) -> Result<PublishedDirectSuccessor, SourceDbImportError> {
    let vehicle_id = binding.vehicle_id;
    apply_seam_forms_to_capture(&mut capture, &mut planned.seam)?;
    let mut next_ordinal = match target {
        SuccessorTarget::Head => store.next_v2_pack_ordinal(run.base_snapshot_id)?,
        SuccessorTarget::WindowRebind { next_ordinal, .. } => next_ordinal,
    };
    let mut parent_digest = planned.parent_digest;
    let mut prior_sequence = planned.from_sequence;
    // Keep one decoded delta batch at a time.  The projection-state spool
    // owns the complete current history; retaining every decoded sparse
    // row here would turn a changed multi-million-row import back into an
    // in-memory history.
    let mut deltas = Vec::new();
    let mut delta_packs = UnpublishedDirectDeltaPacks::new(store, publication_gate);
    // The seam's sync first, as live deltas published with the packs.
    for staged in &planned.seam_sync {
        let to_sequence = prior_sequence
            .checked_add(staged.mutation_count())
            .ok_or(crate::db::StoreError::SequenceTooLarge)?;
        let mut delta = staged.rows()?;
        delta.binding = binding.clone();
        delta.sequence = SequenceRange {
            from_exclusive: prior_sequence,
            to_inclusive: to_sequence,
        };
        delta.parent_digest = parent_digest;
        let built = ProjectionPackWriter::new(store.packs_dir())
            .with_minimum_free_bytes(run.minimum_free_bytes)
            .write_delta(&ProjectionDeltaPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id: run.base_snapshot_id,
                ordinal: next_ordinal,
                delta: &delta,
            })?;
        let chain_digest = canonical_delta_chain_digest(parent_digest, built.metadata.sha256);
        deltas.push(LineageDelta {
            from_sequence: prior_sequence,
            to_sequence,
            parent_chain_digest: parent_digest,
            chain_digest,
            pack_digest: built.metadata.sha256,
            pack: built.metadata.clone(),
        });
        delta_packs.push(built);
        prior_sequence = to_sequence;
        parent_digest = chain_digest;
        next_ordinal = next_ordinal
            .checked_add(1)
            .ok_or(crate::db::StoreError::PackOrdinalTooLarge)?;
    }
    let seam_sequence = prior_sequence;
    direct_delta_rows_from_capture_with_seam(
        &mut capture,
        binding,
        selected_car,
        planned.remaining_delta_packs,
        Some(&mut planned.seam),
        |batch| {
            let to_sequence =
                store.reserve_full_snapshot_sequence_after(publication_gate, vehicle_id, seam_sequence)?;
            if to_sequence <= prior_sequence {
                return Err(crate::db::StoreError::LineageCatalogConflict.into());
            }
            let delta = batch.into_delta(
                binding.clone(),
                SequenceRange {
                    from_exclusive: prior_sequence,
                    to_inclusive: to_sequence,
                },
                parent_digest,
            );
            let built = ProjectionPackWriter::new(store.packs_dir())
                .with_minimum_free_bytes(run.minimum_free_bytes)
                .write_delta(&ProjectionDeltaPackRequest {
                    pack_id: Uuid::new_v4(),
                    snapshot_id: run.base_snapshot_id,
                    ordinal: next_ordinal,
                    delta: &delta,
                })?;
            let chain_digest = canonical_delta_chain_digest(parent_digest, built.metadata.sha256);
            deltas.push(LineageDelta {
                from_sequence: prior_sequence,
                to_sequence,
                parent_chain_digest: parent_digest,
                chain_digest,
                pack_digest: built.metadata.sha256,
                pack: built.metadata.clone(),
            });
            delta_packs.push(built);
            prior_sequence = to_sequence;
            parent_digest = chain_digest;
            next_ordinal = next_ordinal
                .checked_add(1)
                .ok_or(crate::db::StoreError::PackOrdinalTooLarge)?;
            Ok(())
        },
    )?;
    let projection_state = capture.into_state();
    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: prior_sequence,
        },
    )
    .map_err(crate::db::StoreError::Manifest)?;
    match target {
        SuccessorTarget::Head => store.finalize_import_generation_delta_successors_with_staged_seam(
            run.run_id,
            run.source_id,
            vehicle_id,
            binding.selected_car_id,
            run.imported_at_ms,
            &deltas,
            cursor_key,
            &terminal_cursor,
            run.fingerprint,
            run.geofences,
            &projection_state,
            run.materialised_car,
            run.materialised_drives,
            run.materialised_charges,
            planned.seam_digest,
            &planned.seam_sync,
        )?,
        SuccessorTarget::WindowRebind { base, .. } => {
            crate::db::window_rebind_step(crate::db::WindowRebindStep::SeamPacksWritten)?;
            store.finalize_window_rebind_with_staged_seam(
                publication_gate,
                base,
                crate::db::WindowRebindSeam {
                    deltas: &deltas,
                    cursor_key,
                    terminal_cursor: &terminal_cursor,
                    fingerprint: run.fingerprint,
                    projection_state: &projection_state,
                    seam_digest: planned.seam_digest,
                    staged: &planned.seam_sync,
                },
            )?;
        }
    }
    delta_packs.published();
    drop(delta_packs);
    // The import's own rows; the seam's live sync is the Hub's.
    let projected_rows = deltas[planned.seam_sync.len()..].iter().try_fold(0_u64, |total, delta| {
        total
            .checked_add(delta.pack.row_count)
            .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)
    })?;
    Ok(PublishedDirectSuccessor {
        sequence: prior_sequence,
        projected_rows,
    })
}

// The seam's published forms, the 1.4 route and the folded fingerprint
// (M03-T19).
include!("seam_publication.rs");
