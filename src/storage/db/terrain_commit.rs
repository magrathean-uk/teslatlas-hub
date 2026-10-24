// SPDX-License-Identifier: AGPL-3.0-only

/// C9 R16: read the current row with the same decoder used by terrain selection.
fn terrain_commit_position(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    position_id: i64,
) -> Result<Option<(Option<i64>, i64, ProjectionPosition)>, StoreError> {
    let stored: Option<(Option<i64>, i64, String)> = transaction
        .query_row(
            "SELECT drive_id, car_id, position_json FROM materialised_positions
             WHERE vehicle_id = ?1 AND position_id = ?2",
            params![vehicle_id.to_string(), position_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::Query)?;
    stored
        .map(|(drive, car, json)| {
            serde_json::from_str(&json)
                .map(|position| (drive, car, position))
                .map_err(StoreError::DeserializeLifecycleRow)
        })
        .transpose()
}

/// C9 R5 (f), C9 R16: terminal and waiting rows reject prepared work.
fn terrain_commit_provenance_state(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    position_id: i64,
) -> Result<Option<(String, i64, i64)>, StoreError> {
    transaction
        .query_row(
            "SELECT status, attempts, retry_after_ms FROM terrain_elevation_provenance
             WHERE vehicle_id = ?1 AND position_id = ?2",
            params![vehicle_id.to_string(), position_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(StoreError::Query)
}

/// C9 R45: store only provenance that describes this outcome, with its commit clock.
fn write_terrain_batch_provenance(
    transaction: &Transaction<'_>,
    outcome: &TerrainOutcome,
    attempts: i64,
    committed_at_ms: i64,
) -> Result<(), StoreError> {
    let (status, elevation, tile, error_code, retry_after_ms) = match &outcome.kind {
        TerrainOutcomeKind::Filled { elevation_m, tile } => {
            ("success", Some(*elevation_m), Some(tile), None, 0)
        }
        TerrainOutcomeKind::Void { tile, error_code } => ("void", None, Some(tile), *error_code, 0),
        TerrainOutcomeKind::Failed {
            error_code,
            schedule,
            tile,
        } => (
            "failed",
            None,
            if *error_code == "unproven_tile" {
                tile.as_ref()
            } else {
                None
            },
            Some(*error_code),
            committed_at_ms.saturating_add(schedule.delay_ms(attempts)),
        ),
    };
    let candidate = &outcome.candidate;
    let position = &candidate.position;
    transaction
        .execute(
            "INSERT INTO terrain_elevation_provenance(
            vehicle_id, position_id, drive_id, latitude, longitude, elevation_m,
            tile_name, tile_hash, dataset_source, dataset_version, status, error_code,
            attempts, attempted_at_ms, retry_after_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(vehicle_id, position_id) DO UPDATE SET
            drive_id = excluded.drive_id, latitude = excluded.latitude,
            longitude = excluded.longitude, elevation_m = excluded.elevation_m,
            tile_name = excluded.tile_name, tile_hash = excluded.tile_hash,
            dataset_source = excluded.dataset_source, dataset_version = excluded.dataset_version,
            status = excluded.status, error_code = excluded.error_code,
            attempts = excluded.attempts, attempted_at_ms = excluded.attempted_at_ms,
            retry_after_ms = excluded.retry_after_ms",
            params![
                candidate.vehicle_id.to_string(),
                position.id,
                position.drive_id,
                position.latitude,
                position.longitude,
                elevation,
                tile.map(|t| t.tile_name.as_str()),
                tile.map(|t| t.tile_hash.as_str()),
                tile.map(|t| t.dataset_source.as_str()),
                tile.map(|t| t.dataset_version.as_str()),
                status,
                error_code,
                attempts,
                committed_at_ms,
                retry_after_ms
            ],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    // C9 catalogue admission recheck: work order C9-Ca row (ruling CA-R03).
    static TERRAIN_COMMIT_BEFORE_BEGIN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

impl HubStore {
    /// C9 R9, C9 R15, C9 R16: commit one chunk with its derived rows and frontier.
    /// Single-chunk callers and tests use this wrapper;
    /// the pass uses `apply_terrain_batch_for_tidy_run`.
    pub fn apply_terrain_batch(
        &self,
        batch: &TerrainBatch<'_>,
    ) -> Result<TerrainBatchApplied, StoreError> {
        let run = self.terrain_tidy_progress()?.identity();
        self.apply_terrain_batch_for_tidy_run(batch, run)
    }

    /// C9 R16, C9 R58: reject work prepared before the pass's tidy run was re-armed.
    pub fn apply_terrain_batch_for_tidy_run(
        &self,
        batch: &TerrainBatch<'_>,
        run: TerrainTidyRun,
    ) -> Result<TerrainBatchApplied, StoreError> {
        if batch.committed_at_ms < 0 {
            return Err(StoreError::NegativeTimestamp("committed_at_ms"));
        }
        let mut connection = self.open()?;
        #[cfg(test)]
        TERRAIN_COMMIT_BEFORE_BEGIN.with(|slot| {
            if let Some(hook) = slot.borrow_mut().take() {
                hook();
            }
        });
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        admit_writable_catalogue(&transaction)?;
        let mut applied = TerrainBatchApplied::default();
        if terrain_tidy_run_in_transaction(&transaction)? != run {
            applied.rearmed = true;
            return Ok(applied);
        }
        let vehicle = batch.vehicle_id;
        let vehicle_key = vehicle.to_string();
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM vehicles WHERE vehicle_id = ?1)",
                [&vehicle_key],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        if !exists {
            applied.stale_dropped = u32::try_from(batch.outcomes.len() + batch.retire.len())
                .map_err(|_| StoreError::InvalidStoredCount)?;
            return Ok(applied);
        }
        let boundary = terrain_merged_boundary(
            terrain_stored_boundary(&transaction, vehicle)?,
            terrain_watermark_boundary(&transaction, vehicle)?,
        );
        let mut eligibility = HashMap::new();
        let mut filled_drives = Vec::new();
        let mut seen_drives = HashSet::new();
        let mut cursor_to = batch.cursor_to;
        for outcome in batch.outcomes {
            let candidate = &outcome.candidate;
            let id = candidate.position.id;
            let Some((drive, car, position)) = terrain_commit_position(&transaction, vehicle, id)?
            else {
                applied.stale_dropped += 1;
                continue;
            };
            let Some(drive_id) = drive else {
                applied.stale_dropped += 1;
                continue;
            };
            let prior = terrain_commit_provenance_state(&transaction, vehicle, id)?;
            let queue_matches = if outcome.forward {
                prior.is_none()
            } else {
                prior.as_ref().is_some_and(|(status, _, retry)| {
                    status == "failed" && *retry <= batch.committed_at_ms
                })
            };
            if candidate.vehicle_id != vehicle
                || drive != candidate.position.drive_id
                || !queue_matches
                || !terrain_qualifies(
                    &transaction,
                    vehicle,
                    drive,
                    &position,
                    boundary,
                    &mut eligibility,
                )?
            {
                applied.stale_dropped += 1;
                continue;
            }
            if position.latitude != candidate.position.latitude
                || position.longitude != candidate.position.longitude
            {
                applied.stale_held.push(id);
                if outcome.forward {
                    cursor_to = cursor_to.map(|cursor| cursor.min(id.saturating_sub(1)));
                }
                continue;
            }
            let attempts = prior.map_or(1, |(_, attempts, _)| attempts.saturating_add(1));
            match &outcome.kind {
                TerrainOutcomeKind::Filled { elevation_m, .. } => {
                    transaction
                        .execute(
                            "UPDATE materialised_positions
                         SET position_json = json_set(position_json, '$.elevation', ?3)
                         WHERE vehicle_id = ?1 AND position_id = ?2",
                            params![&vehicle_key, id, elevation_m],
                        )
                        .map_err(StoreError::LifecycleWrite)?;
                    let payload: String = transaction
                        .query_row(
                            "SELECT position_json FROM materialised_positions
                         WHERE vehicle_id = ?1 AND position_id = ?2",
                            params![&vehicle_key, id],
                            |row| row.get(0),
                        )
                        .map_err(StoreError::Query)?;
                    record_sync_mutation_in_transaction(
                        &transaction,
                        vehicle,
                        "position",
                        id,
                        car,
                        "upsert",
                        &payload,
                    )?;
                    if seen_drives.insert(drive_id) {
                        filled_drives.push(drive_id);
                    }
                    applied.filled += 1;
                }
                TerrainOutcomeKind::Void { error_code, .. } => {
                    applied.void += 1;
                    applied.out_of_range += u32::from(*error_code == Some("out_of_range"));
                }
                TerrainOutcomeKind::Failed { .. } => applied.failed += 1,
            }
            write_terrain_batch_provenance(&transaction, outcome, attempts, batch.committed_at_ms)?;
        }
        // C9 R10: a retirement is conditional on the retry still being invalid and due.
        for &id in batch.retire {
            let prior = terrain_commit_provenance_state(&transaction, vehicle, id)?;
            if !prior.is_some_and(|(status, _, retry)| {
                status == "failed" && retry <= batch.committed_at_ms
            }) {
                continue;
            }
            if let Some((drive, _, position)) = terrain_commit_position(&transaction, vehicle, id)?
                && terrain_qualifies(
                    &transaction,
                    vehicle,
                    drive,
                    &position,
                    boundary,
                    &mut eligibility,
                )?
            {
                continue;
            }
            applied.retired += transaction
                .execute(
                    "DELETE FROM terrain_elevation_provenance
                 WHERE vehicle_id = ?1 AND position_id = ?2",
                    params![&vehicle_key, id],
                )
                .map_err(StoreError::LifecycleWrite)? as u32;
        }
        // C9 R39: every filled drive is totalled after the last position mutation.
        for drive_id in filled_drives {
            if recompute_terrain_drive_with_change(&transaction, vehicle, drive_id)? {
                applied.drives_changed += 1;
            }
        }
        if applied.filled > 0 {
            mark_export_dirty_in_transaction(&transaction, vehicle)?;
        }
        if let Some(cursor) = cursor_to {
            let changed = transaction.execute(
                "INSERT INTO terrain_enrichment_state(vehicle_id, cursor_position_id, updated_at_ms)
                 SELECT ?1, ?2, ?3 WHERE ?2 > 0
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    cursor_position_id = excluded.cursor_position_id,
                    updated_at_ms = excluded.updated_at_ms
                 WHERE excluded.cursor_position_id > terrain_enrichment_state.cursor_position_id",
                params![&vehicle_key, cursor, batch.committed_at_ms],
            ).map_err(StoreError::LifecycleWrite)?;
            if changed > 0 {
                applied.cursor_written = Some(cursor);
            }
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(applied)
    }
}
