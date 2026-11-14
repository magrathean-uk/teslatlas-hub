// SPDX-License-Identifier: AGPL-3.0-only

// The negotiated 1.4 route's physical publication (M03-T21, DEC-117).

/// Publishes one import's physical 1.4 head after its planned lineage.
fn publish_physical_route(
    store: &HubStore,
    cursor_key: &CursorKey,
    binding: ProjectionBinding,
    stage: Option<crate::source_db_stage::OwnedSourceDbStage>,
    publication_gate: &PublicationGate,
    _seam: Option<&crate::import::source_db::seam::SeamPackPlan>,
) -> Result<Option<crate::physical_publication::PhysicalV3Publication>, SourceDbImportError> {
    let stage = physical_stage_for_publication(store, binding.vehicle_id, stage)?;
    Ok(stage
        .map(|stage| {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|value| i64::try_from(value.as_millis()).ok())
                .ok_or(crate::physical_publication::SourceDbPhysicalPublicationError::Clock)?;
            crate::physical_publication::publish_sealed_physical_v3_stage_with_gate_at(
                store,
                cursor_key,
                binding,
                stage.take(),
                publication_gate,
                now_ms,
            )
        })
        .transpose()?)
}
