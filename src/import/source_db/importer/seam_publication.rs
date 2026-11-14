// SPDX-License-Identifier: AGPL-3.0-only

// The pack side of the seam repair (M03-T19, review REV-CX-M03-T16): the
// published forms a successor's state holds and the source fingerprint of a
// vehicle with the seam.

/// What the packs publish of one source position, charge or charge sample
/// under `packs` (findings 2 and 3): left out, as recorded, or in the form
/// the seam gives it (another parent, or widened bounds).
fn seam_form(
    packs: &crate::import::source_db::seam::SeamPackPlan,
    entity: SourceDbProjectionStateEntity,
    id: i64,
    car_id: i64,
    payload: &[u8],
) -> Result<crate::import_projection_state::SeamForm, SourceDbProjectionStateError> {
    use crate::import::source_db::seam::{ChildRow, PackedChild, PackedSession};
    use crate::import_projection_state::{SeamForm, canonical_payload_and_digest};
    match entity {
        SourceDbProjectionStateEntity::Position => {
            let mut row: ProjectionPosition =
                serde_json::from_slice(payload).map_err(SourceDbProjectionStateError::SerializeRow)?;
            let child = ChildRow {
                id,
                at_ms: row.date_ms,
                parent: row.drive_id,
            };
            match packs.position(child) {
                PackedChild::Skip => Ok(SeamForm::LeftOut),
                PackedChild::Keep { parent } if parent == row.drive_id => Ok(SeamForm::Recorded),
                PackedChild::Keep { parent } => {
                    row.drive_id = parent;
                    let (payload, digest) = canonical_payload_and_digest(entity, id, car_id, &row)?;
                    Ok(SeamForm::Published { payload, digest })
                }
            }
        }
        SourceDbProjectionStateEntity::ChargeSample => {
            let mut row: ProjectionChargeSample =
                serde_json::from_slice(payload).map_err(SourceDbProjectionStateError::SerializeRow)?;
            let child = ChildRow {
                id,
                at_ms: row.timestamp_ms,
                parent: Some(row.charge_process_id),
            };
            match packs.charge_sample(child) {
                PackedChild::Skip => Ok(SeamForm::LeftOut),
                PackedChild::Keep { parent } => {
                    let parent = parent.unwrap_or(row.charge_process_id);
                    if parent == row.charge_process_id {
                        return Ok(SeamForm::Recorded);
                    }
                    row.charge_process_id = parent;
                    let (payload, digest) = canonical_payload_and_digest(entity, id, car_id, &row)?;
                    Ok(SeamForm::Published { payload, digest })
                }
            }
        }
        SourceDbProjectionStateEntity::Charge => {
            let row: ProjectionCharge =
                serde_json::from_slice(payload).map_err(SourceDbProjectionStateError::SerializeRow)?;
            match packs.charge(id) {
                // A charge the plan never saw stays as recorded; the packs
                // refuse it when they read it.
                None | Some(PackedSession::Keep { bounds: None }) => Ok(SeamForm::Recorded),
                Some(PackedSession::Skip) => Ok(SeamForm::LeftOut),
                Some(PackedSession::Keep { bounds }) => {
                    let planned = crate::db::planned_import_charge(&row, bounds);
                    if planned == row {
                        return Ok(SeamForm::Recorded);
                    }
                    let (payload, digest) =
                        canonical_payload_and_digest(entity, id, car_id, &planned)?;
                    Ok(SeamForm::Published { payload, digest })
                }
            }
        }
        _ => Ok(SeamForm::Recorded),
    }
}

/// Holds the seam's published forms in a successor's state (finding 2),
/// once, before any page is read: the plan must be the one the capture's
/// lens was read for, or the store changed after the source was read.
fn apply_seam_forms_to_capture(
    capture: &mut SourceDbProjectionStateCapture,
    seam: &mut DeltaSeam,
) -> Result<(), SourceDbImportError> {
    let plan = seam.packs.digest();
    match capture.seam_forms() {
        Some(applied) if applied == plan => return Ok(()),
        Some(_) => return Err(StoreError::ImportGenerationConflict.into()),
        None => {}
    }
    if capture
        .seam_lens_coverage()
        .is_some_and(|coverage| coverage != seam.packs.coverage())
    {
        return Err(StoreError::ImportGenerationConflict.into());
    }
    let packs = seam.packs.clone();
    let scan = seam.packs.changes_published_forms();
    let applied = capture.apply_seam_forms(plan, scan, &mut |entity, id, car_id, payload| {
        seam_form(&packs, entity, id, car_id, payload)
    })?;
    seam.skipped.positions = seam.skipped.positions.saturating_add(applied.positions_left_out);
    seam.skipped.charges = seam.skipped.charges.saturating_add(applied.charges_left_out);
    seam.skipped.charge_samples = seam
        .skipped
        .charge_samples
        .saturating_add(applied.charge_samples_left_out);
    for (entity, id, car_id) in applied.withdrawn {
        let entity = match entity {
            SourceDbProjectionStateEntity::Position => ProjectionDeltaEntity::Position,
            SourceDbProjectionStateEntity::Charge => ProjectionDeltaEntity::Charge,
            SourceDbProjectionStateEntity::ChargeSample => ProjectionDeltaEntity::ChargeSample,
            _ => return Err(SourceDbImportError::InvalidDirectDeltaTombstone),
        };
        seam.withdrawn.push(ProjectionTombstone { entity, id, car_id });
    }
    Ok(())
}

/// The source fingerprint a successor compares and records (finding 2): for
/// a vehicle with seam history it also covers the seam's pack plan, so an
/// unchanged source still publishes when the plan changed (a new window) and
/// an unchanged source and plan publish nothing. A plan that cannot be worked
/// out leaves the source fingerprint as it is; the publication refuses later.
#[cfg(test)]
fn seam_folded_fingerprint(
    store: &HubStore,
    publication_gate: &PublicationGate,
    vehicle_id: Uuid,
    selected_car_id: i64,
    drives: &[ProjectionDrive],
    charges: &[ProjectionCharge],
    fingerprint: Sha256Digest,
) -> Sha256Digest {
    seam_folded_fingerprint_and_plan(
        store,
        publication_gate,
        vehicle_id,
        selected_car_id,
        drives,
        charges,
        fingerprint,
    )
    .0
}

/// [`seam_folded_fingerprint`], with the pack plan it folded in: the plan an
/// unchanged source's physical 1.4 head is planned with (M03-T21), since the
/// same digest means the plan the last publication proved.
fn seam_folded_fingerprint_and_plan(
    store: &HubStore,
    publication_gate: &PublicationGate,
    vehicle_id: Uuid,
    selected_car_id: i64,
    drives: &[ProjectionDrive],
    charges: &[ProjectionCharge],
    fingerprint: Sha256Digest,
) -> (Sha256Digest, Option<crate::import::source_db::seam::SeamPackPlan>) {
    match store.dry_run_import_seam_ahead_of_packs(
        publication_gate,
        vehicle_id,
        selected_car_id,
        drives,
        charges,
    ) {
        Ok(decided) if decided.seam_history => {
            let mut hash = Sha256::new();
            hash.update(b"teslatlas-hub source fingerprint with the import seam v1\n");
            hash.update(fingerprint.as_bytes());
            hash.update(decided.ahead.packs.digest());
            (
                Sha256Digest::from_bytes(hash.finalize().into()),
                Some(decided.ahead.packs),
            )
        }
        _ => (fingerprint, None),
    }
}

// The negotiated 1.4 route's physical publication (M03-T21).
include!("physical_route.rs");
