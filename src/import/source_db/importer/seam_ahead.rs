// SPDX-License-Identifier: AGPL-3.0-only

/// The seam plan applied to one successor's delta rows (design M03-SEAM-R3,
/// section 4): the packs carry the planned source rows only. A skipped drive
/// is left out, a packed drive carries the bounds the store writes, a source
/// position inside coverage is left out and one whose drive was skipped is
/// packed under the drive that stands for it. Packed drives whose stored row
/// is not yet the planned row are restated even when the source did not
/// change them, so the packs and the store hold the same drives.
#[derive(Debug, Clone)]
struct DeltaSeam {
    packs: crate::import::source_db::seam::SeamPackPlan,
    restated: Vec<ProjectionDrive>,
    emitted_drives: HashSet<i64>,
    skipped: SourceDbSeamSkipped,
}

impl DeltaSeam {
    fn new(ahead: &crate::db::ImportSeamAhead, drives: &[ProjectionDrive]) -> Self {
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
        Self {
            packs: ahead.packs.clone(),
            restated,
            emitted_drives: HashSet::new(),
            skipped: SourceDbSeamSkipped::default(),
        }
    }
}

/// A direct successor after the seam went ahead of its packs.
struct PlannedDirectSuccessor {
    seam: DeltaSeam,
    seam_digest: [u8; 32],
    from_sequence: u64,
    parent_digest: Sha256Digest,
    existing: crate::protocol::LineageManifestV2,
    existing_pack_count: usize,
    remaining_delta_packs: usize,
}

/// Applies the import seam ahead of a direct successor's packs (design
/// M03-SEAM-R3, section 4) and publishes the sync mutations it made, so every
/// head a client can read holds each session once: a Hub drive the source
/// replaces is tombstoned at a head before the packs that carry its
/// replacement, and an earlier import's copy of a skipped drive goes the
/// same way. The packs then chain onto the head that publication left.
fn plan_direct_successor(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    selected_car_id: i64,
    drives: &[ProjectionDrive],
) -> Result<PlannedDirectSuccessor, SourceDbImportError> {
    let ahead =
        store.apply_import_seam_ahead_of_packs(publication_gate, vehicle_id, selected_car_id, drives)?;
    publish_sync_mutations_ahead_of_packs(store, publication_gate, cursor_key, vehicle_id)?;
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
    let remaining_delta_packs = ProtocolLimits::default()
        .max_chunks
        .checked_sub(existing_pack_count)
        .ok_or(crate::db::StoreError::LineageCatalogConflict)?;
    Ok(PlannedDirectSuccessor {
        seam: DeltaSeam::new(&ahead, drives),
        seam_digest: ahead.packs.digest(),
        from_sequence,
        parent_digest: head_digest,
        existing,
        existing_pack_count,
        remaining_delta_packs,
    })
}

/// Publishes every unpublished sync mutation of the vehicle, the seam's
/// among them, as live deltas under the publication gate the importer holds.
fn publish_sync_mutations_ahead_of_packs(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
) -> Result<(), SourceDbImportError> {
    const CLAIM_LIMIT: usize = 10_000;
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
            CLAIM_LIMIT,
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
}

/// What a planned direct successor published.
#[derive(Debug)]
struct PublishedDirectSuccessor {
    sequence: u64,
    projected_rows: u64,
}

/// Writes a planned direct successor's delta packs (the planned source rows
/// only) and publishes them; publication plans the seam again and refuses a
/// different plan.
#[allow(clippy::too_many_arguments)]
fn publish_planned_direct_successor(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    run: PlannedSuccessorRun<'_>,
    binding: &ProjectionBinding,
    selected_car: &ProjectionCar,
    mut capture: SourceDbProjectionStateCapture,
    planned: &mut PlannedDirectSuccessor,
) -> Result<PublishedDirectSuccessor, SourceDbImportError> {
    let vehicle_id = binding.vehicle_id;
    let mut next_ordinal = store.next_v2_pack_ordinal(run.base_snapshot_id)?;
    let mut parent_digest = planned.parent_digest;
    let mut prior_sequence = planned.from_sequence;
    // Keep one decoded delta batch at a time.  The projection-state spool
    // owns the complete current history; retaining every decoded sparse
    // row here would turn a changed multi-million-row import back into an
    // in-memory history.
    let mut deltas = Vec::new();
    let mut delta_packs = UnpublishedDirectDeltaPacks::new(store, publication_gate);
    direct_delta_rows_from_capture_with_seam(
        &mut capture,
        binding,
        selected_car,
        planned.remaining_delta_packs,
        Some(&mut planned.seam),
        |batch| {
            let to_sequence = store.reserve_next_full_snapshot_sequence(publication_gate, vehicle_id)?;
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
    store.finalize_import_generation_delta_successors_with_projection_state_and_seam_plan(
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
        planned.seam_digest,
    )?;
    delta_packs.published();
    drop(delta_packs);
    let projected_rows = deltas.iter().try_fold(0_u64, |total, delta| {
        total
            .checked_add(delta.pack.row_count)
            .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)
    })?;
    Ok(PublishedDirectSuccessor {
        sequence: prior_sequence,
        projected_rows,
    })
}
