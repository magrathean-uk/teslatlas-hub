// SPDX-License-Identifier: AGPL-3.0-only

// A direct successor's typed delta rows, read from its projection-state
// capture in bounded batches (M03-T24 split).

#[derive(Debug, Default)]
struct DirectDeltaRows {
    cars: Vec<ProjectionCar>,
    drives: Vec<ProjectionDrive>,
    positions: Vec<ProjectionPosition>,
    charges: Vec<ProjectionCharge>,
    charge_samples: Vec<ProjectionChargeSample>,
    states: Vec<ProjectionState>,
    updates: Vec<ProjectionUpdate>,
    tombstones: Vec<ProjectionTombstone>,
    row_count: u64,
    payload_bytes: u64,
}

impl DirectDeltaRows {
    fn is_empty(&self) -> bool {
        self.row_count == 0
    }

    fn into_delta(
        self,
        binding: ProjectionBinding,
        sequence: SequenceRange,
        parent_digest: Sha256Digest,
    ) -> ProjectionDelta {
        ProjectionDelta {
            binding,
            sequence,
            parent_digest,
            cars: self.cars,
            car_settings: Vec::new(),
            drives: self.drives,
            positions: self.positions,
            charges: self.charges,
            charge_samples: self.charge_samples,
            states: self.states,
            updates: self.updates,
            tombstones: self.tombstones,
        }
    }

    fn next_would_exceed(&self, payload_bytes: u64) -> Result<bool, SourceDbImportError> {
        let next_rows = self
            .row_count
            .checked_add(1)
            .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)?;
        let next_bytes = self
            .payload_bytes
            .checked_add(payload_bytes)
            .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)?;
        Ok(next_rows > DIRECT_DELTA_BATCH_ROWS || next_bytes > DIRECT_DELTA_BATCH_PAYLOAD_BYTES)
    }

    fn reserve(&mut self, payload_bytes: u64) -> Result<(), SourceDbImportError> {
        self.row_count = self
            .row_count
            .checked_add(1)
            .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)?;
        self.payload_bytes = self
            .payload_bytes
            .checked_add(payload_bytes)
            .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)?;
        Ok(())
    }
}

/// Visit bounded sparse row batches from a sealed current-run spool. The
/// visitor owns each decoded batch before the next page is read, keeping the
/// importer bounded even when every source row changed. It also enforces the
/// remaining protocol pack capacity before an unbounded number of candidate
/// pack files can be constructed.
#[cfg(test)]
fn direct_delta_capacity_exhausted(
    capture: &mut SourceDbProjectionStateCapture,
    binding: &ProjectionBinding,
    car: &ProjectionCar,
    maximum_batches: usize,
) -> Result<bool, SourceDbImportError> {
    direct_delta_capacity_exhausted_with_seam(capture, binding, car, maximum_batches, None)
}

fn direct_delta_capacity_exhausted_with_seam(
    capture: &mut SourceDbProjectionStateCapture,
    binding: &ProjectionBinding,
    car: &ProjectionCar,
    maximum_batches: usize,
    seam: Option<&DeltaSeam>,
) -> Result<bool, SourceDbImportError> {
    // A dry run: the seam's counts belong to the real run.
    let mut seam = seam.cloned();
    match direct_delta_rows_from_capture_with_seam(
        capture,
        binding,
        car,
        maximum_batches,
        seam.as_mut(),
        |_| Ok(()),
    ) {
        Ok(()) => Ok(false),
        Err(SourceDbImportError::DirectDeltaBatchLimitExceeded { .. }) => Ok(true),
        Err(error) => Err(error),
    }
}

fn direct_delta_rows_from_capture<F>(
    capture: &mut SourceDbProjectionStateCapture,
    binding: &ProjectionBinding,
    metadata_only_car: &ProjectionCar,
    maximum_batches: usize,
    visit: F,
) -> Result<(), SourceDbImportError>
where
    F: FnMut(DirectDeltaRows) -> Result<(), SourceDbImportError>,
{
    direct_delta_rows_from_capture_with_seam(
        capture,
        binding,
        metadata_only_car,
        maximum_batches,
        None,
        visit,
    )
}

/// As [`direct_delta_rows_from_capture`], with the seam plan applied to the
/// rows when one is given.
fn direct_delta_rows_from_capture_with_seam<F>(
    capture: &mut SourceDbProjectionStateCapture,
    binding: &ProjectionBinding,
    metadata_only_car: &ProjectionCar,
    maximum_batches: usize,
    mut seam: Option<&mut DeltaSeam>,
    mut visit: F,
) -> Result<(), SourceDbImportError>
where
    F: FnMut(DirectDeltaRows) -> Result<(), SourceDbImportError>,
{
    if maximum_batches == 0 {
        return Err(SourceDbImportError::DirectDeltaBatchLimitExceeded {
            maximum: maximum_batches,
        });
    }
    let mut emitted_batches = 0_usize;
    let mut current = DirectDeltaRows::default();
    let mut changed_after = None;
    loop {
        let page = capture.changed_page_with_payload_limit(
            changed_after,
            DIRECT_DELTA_PAGE_ROWS,
            DIRECT_DELTA_BATCH_PAYLOAD_BYTES,
        )?;
        for row in page.rows {
            let payload_bytes = u64::try_from(row.canonical_payload.len())
                .map_err(|_| SourceDbImportError::DirectDeltaBatchOverflow)?;
            if payload_bytes > DIRECT_DELTA_BATCH_PAYLOAD_BYTES {
                return Err(SourceDbImportError::DirectDeltaRowTooLarge { payload_bytes });
            }
            if current.next_would_exceed(payload_bytes)? && !current.is_empty() {
                emit_direct_delta_batch(
                    &mut emitted_batches,
                    maximum_batches,
                    current,
                    &mut visit,
                )?;
                current = DirectDeltaRows::default();
            }
            append_changed_direct_delta_row(&mut current, &row, binding, seam.as_deref_mut())?;
        }
        match page.next_after {
            Some(next_after) => changed_after = Some(next_after),
            None => break,
        }
    }

    // Packed sessions the source did not change but the plan restates.
    if let Some(seam) = seam.as_deref() {
        seam.append_restated(
            &mut current,
            &mut emitted_batches,
            maximum_batches,
            binding,
            &mut visit,
        )?;
    }

    let mut tombstone_after = None;
    let withdrawn = seam.as_deref().map(|seam| seam.withdrawn.clone()).unwrap_or_default();
    let mut withdrawn = withdrawn.into_iter();
    loop {
        let (mut rows, next_after) =
            capture.tombstone_page(tombstone_after, DIRECT_DELTA_PAGE_ROWS)?;
        if next_after.is_none() {
            // Rows the seam withdrew from clients, then (M03-T19) never a
            // session the store keeps on the Hub's side.
            rows.extend(withdrawn.by_ref());
        }
        rows.retain(|row| !seam.as_deref().is_some_and(|seam| seam.retains(row)));
        for row in rows {
            let payload_bytes = u64::try_from(
                serde_json::to_vec(&row)
                    .map_err(SourceDbImportError::SerializeDirectDeltaRow)?
                    .len(),
            )
            .map_err(|_| SourceDbImportError::DirectDeltaBatchOverflow)?;
            if current.next_would_exceed(payload_bytes)? && !current.is_empty() {
                emit_direct_delta_batch(
                    &mut emitted_batches,
                    maximum_batches,
                    current,
                    &mut visit,
                )?;
                current = DirectDeltaRows::default();
            }
            if row.id <= 0
                || row.car_id != binding.selected_car_id
                || matches!(row.entity, ProjectionDeltaEntity::Car)
            {
                return Err(SourceDbImportError::InvalidDirectDeltaTombstone);
            }
            current.reserve(payload_bytes)?;
            current.tombstones.push(row);
        }
        match next_after {
            Some(next_after) => tombstone_after = Some(next_after),
            None => break,
        }
    }

    if current.is_empty() && emitted_batches == 0 {
        let payload_bytes = u64::try_from(
            serde_json::to_vec(metadata_only_car)
                .map_err(SourceDbImportError::SerializeDirectDeltaRow)?
                .len(),
        )
        .map_err(|_| SourceDbImportError::DirectDeltaBatchOverflow)?;
        current.reserve(payload_bytes)?;
        current.cars.push(metadata_only_car.clone());
    }
    if !current.is_empty() {
        emit_direct_delta_batch(&mut emitted_batches, maximum_batches, current, &mut visit)?;
    }
    Ok(())
}

fn emit_direct_delta_batch<F>(
    emitted_batches: &mut usize,
    maximum_batches: usize,
    batch: DirectDeltaRows,
    visit: &mut F,
) -> Result<(), SourceDbImportError>
where
    F: FnMut(DirectDeltaRows) -> Result<(), SourceDbImportError>,
{
    if *emitted_batches >= maximum_batches {
        return Err(SourceDbImportError::DirectDeltaBatchLimitExceeded {
            maximum: maximum_batches,
        });
    }
    visit(batch)?;
    *emitted_batches = emitted_batches
        .checked_add(1)
        .ok_or(SourceDbImportError::DirectDeltaBatchOverflow)?;
    Ok(())
}

fn append_changed_direct_delta_row(
    batch: &mut DirectDeltaRows,
    row: &crate::import_projection_state::SourceDbProjectionStateChangedRow,
    binding: &ProjectionBinding,
    seam: Option<&mut DeltaSeam>,
) -> Result<(), SourceDbImportError> {
    use crate::import::source_db::seam::{ChildRow, PackedChild, PackedSession};
    let payload_bytes = u64::try_from(row.canonical_payload.len())
        .map_err(|_| SourceDbImportError::DirectDeltaBatchOverflow)?;
    match row.state.entity {
        SourceDbProjectionStateEntity::Car => {
            let value: ProjectionCar = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.cars.push(value);
        }
        SourceDbProjectionStateEntity::Drive => {
            let mut value: ProjectionDrive = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            if let Some(seam) = seam {
                match seam.packs.drive(value.id) {
                    None => return Err(SourceDbImportError::InvalidDirectDeltaRow),
                    Some(PackedSession::Skip) => {
                        seam.skipped.drives = seam.skipped.drives.saturating_add(1);
                        return Ok(());
                    }
                    Some(PackedSession::Keep { bounds }) => {
                        value = crate::db::planned_import_drive(&value, bounds);
                        seam.emitted_drives.insert(value.id);
                    }
                }
            }
            batch.reserve(payload_bytes)?;
            batch.drives.push(value);
        }
        SourceDbProjectionStateEntity::Position => {
            let mut value: ProjectionPosition = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            if let Some(seam) = seam {
                let child = ChildRow {
                    id: value.id,
                    at_ms: value.date_ms,
                    parent: value.drive_id,
                };
                match seam.packs.position(child) {
                    PackedChild::Skip => {
                        seam.skipped.positions = seam.skipped.positions.saturating_add(1);
                        return Ok(());
                    }
                    PackedChild::Keep { parent } => value.drive_id = parent,
                }
            }
            batch.reserve(payload_bytes)?;
            batch.positions.push(value);
        }
        SourceDbProjectionStateEntity::Charge => {
            let mut value: ProjectionCharge = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            if seam.map(|seam| seam.pack_charge(&mut value)).transpose()? == Some(false) {
                return Ok(());
            }
            batch.reserve(payload_bytes)?;
            batch.charges.push(value);
        }
        SourceDbProjectionStateEntity::ChargeSample => {
            let mut value: ProjectionChargeSample = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, row.state.car_id, row, binding)?;
            if seam.is_some_and(|seam| !seam.pack_charge_sample(&mut value)) {
                return Ok(());
            }
            batch.reserve(payload_bytes)?;
            batch.charge_samples.push(value);
        }
        SourceDbProjectionStateEntity::State => {
            let value: ProjectionState = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.states.push(value);
        }
        SourceDbProjectionStateEntity::Update => {
            let value: ProjectionUpdate = serde_json::from_slice(&row.canonical_payload)
                .map_err(SourceDbImportError::DecodeDirectDeltaRow)?;
            validate_direct_delta_identity(value.id, value.car_id, row, binding)?;
            batch.reserve(payload_bytes)?;
            batch.updates.push(value);
        }
    }
    Ok(())
}

fn validate_direct_delta_identity(
    id: i64,
    car_id: i64,
    row: &crate::import_projection_state::SourceDbProjectionStateChangedRow,
    binding: &ProjectionBinding,
) -> Result<(), SourceDbImportError> {
    if id != row.state.id
        || car_id != row.state.car_id
        || car_id != binding.selected_car_id
        || id <= 0
    {
        return Err(SourceDbImportError::InvalidDirectDeltaRow);
    }
    Ok(())
}
