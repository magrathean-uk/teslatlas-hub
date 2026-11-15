// SPDX-License-Identifier: AGPL-3.0-only

// The first import onto a window-only Hub (DEC-150): its base in place of
// the collector's lineage and the seam on it, published in one catalogue
// transaction (DEC-151 follow-up, M03-T24).

/// Publishes a window rebind: the import's signed base `manifest` in place
/// of the collector's lineage `expected`, and the seam on it from the
/// rebind's own sealed capture, in one catalogue transaction. The seam is
/// planned against the uncommitted rebind (a rehearsal the store rolls
/// back); its forms and the Hub's rows are then the only deltas of the
/// base, written as packs and published with it. Whatever interrupts it,
/// the store holds either the old window lineage or the complete new one.
/// `run.fingerprint` is the source's; the seam is folded into it.
#[allow(clippy::too_many_arguments)]
fn publish_window_rebind(
    store: &HubStore,
    publication_gate: &PublicationGate,
    cursor_key: &CursorKey,
    mut run: PlannedSuccessorRun<'_>,
    expected: &crate::protocol::LineageManifestV2,
    manifest: &crate::protocol::SyncManifest,
    binding: &ProjectionBinding,
    selected_car: &ProjectionCar,
    mut capture: SourceDbProjectionStateCapture,
) -> Result<(PublishedDirectSuccessor, DeltaSeam), SourceDbImportError> {
    if run.base_snapshot_id != manifest.snapshot_id {
        return Err(crate::db::StoreError::LineageCatalogConflict.into());
    }
    let base = crate::db::WindowRebindBase {
        expected,
        run_id: run.run_id,
        source_id: run.source_id,
        vehicle_id: binding.vehicle_id,
        car_id: binding.selected_car_id,
        updated_at_ms: run.imported_at_ms,
        manifest,
        geofences: run.geofences,
        binding,
        car: run.materialised_car,
        drives: run.materialised_drives,
        charges: run.materialised_charges,
    };
    // The base state is transferred without the seam rows its lens held.
    let held = capture.take_seam_retained()?;
    let stage = store.stage_window_rebind_seam(publication_gate, &base, capture.state())?;
    // The base's durable state, as the store will hold it in the same
    // transaction, is the prior of the seam's capture.
    let prior = capture.state().frozen_prior()?;
    capture.restore_seam_retained(held)?;
    crate::db::window_rebind_step(crate::db::WindowRebindStep::SeamStaged)?;
    let max_chunks = ProtocolLimits::default().max_chunks;
    let used = stage.base_pack_count;
    let live_deltas = stage.sync.len();
    let remaining = max_chunks.saturating_sub(used.saturating_add(live_deltas));
    let seam = DeltaSeam::new(&stage.decided.ahead, run.materialised_drives, run.materialised_charges);
    let mut capture = capture.into_rebound_successor(Box::new(prior));
    if direct_delta_capacity_exhausted_with_seam(&mut capture, binding, selected_car, remaining, Some(&seam))? {
        // The base's packs leave no room for the seam's deltas; the window
        // lineage stays.
        return Err(SourceDbImportError::ImportBaseReplacementRefused { used, limit: max_chunks });
    }
    (run.fingerprint, _) = fold_seam_fingerprint(run.fingerprint, &stage.decided);
    let mut planned = PlannedDirectSuccessor {
        seam,
        seam_digest: stage.decided.ahead.packs.digest(),
        seam_sync: stage.sync,
        from_sequence: stage.head_sequence,
        parent_digest: stage.head_digest,
        remaining_delta_packs: remaining,
    };
    let published = publish_planned_direct_successor_to(
        store,
        publication_gate,
        cursor_key,
        run,
        binding,
        selected_car,
        capture,
        &mut planned,
        SuccessorTarget::WindowRebind {
            base: &base,
            next_ordinal: stage.next_ordinal,
        },
    )?;
    Ok((published, planned.seam))
}
