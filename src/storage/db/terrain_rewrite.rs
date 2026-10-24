// SPDX-License-Identifier: AGPL-3.0-only

/// C9 R54: an ordered, transaction-local set containing drive ids only.
#[derive(Default)]
struct TerrainRecomputeRequests {
    ids: Vec<i64>,
    seen: HashSet<i64>,
}

impl TerrainRecomputeRequests {
    fn extend(&mut self, ids: impl IntoIterator<Item = i64>) {
        for id in ids {
            if self.seen.insert(id) {
                self.ids.push(id);
            }
        }
    }
}

/// C9 R54: requeue the catalogue key and link without counting an acquisition.
fn requeue_rewritten_terrain(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    position_id: i64,
    drive_id: Option<i64>,
    position: &ProjectionPosition,
    now_ms: i64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "UPDATE terrain_elevation_provenance SET drive_id = ?3,
            latitude = ?4, longitude = ?5, status = 'failed',
            error_code = 'position_rewritten', retry_after_ms = ?6,
            elevation_m = NULL, tile_name = NULL, tile_hash = NULL,
            dataset_source = NULL, dataset_version = NULL
         WHERE vehicle_id = ?1 AND position_id = ?2",
            params![
                vehicle_id.to_string(),
                position_id,
                drive_id,
                position.latitude,
                position.longitude,
                now_ms
            ],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

/// C9 R54: correct the row immediately and leave all drive totals to the caller.
pub(crate) fn terrain_after_position_write_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    position_id: i64,
    now_ms: i64,
) -> Result<TerrainRewriteApplied, StoreError> {
    let vehicle_key = vehicle_id.to_string();
    let prior: Option<(i64, f64, f64, Option<i64>, String)> = transaction
        .query_row(
            "SELECT drive_id, latitude, longitude, elevation_m, status
         FROM terrain_elevation_provenance WHERE vehicle_id = ?1 AND position_id = ?2",
            params![&vehicle_key, position_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(StoreError::Query)?;
    let mut applied = TerrainRewriteApplied {
        outcome: TerrainRewrite::Unchanged,
        recompute_drive_ids: Vec::new(),
    };
    let Some((old_drive, latitude, longitude, elevation, status)) = prior else {
        return Ok(applied);
    };
    let current = terrain_commit_position(transaction, vehicle_id, position_id)?;
    let mut requeue = false;
    if let Some((drive, _, mut position)) = current {
        let drive_exists = match drive {
            Some(id) => transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM materialised_drives
                 WHERE vehicle_id = ?1 AND drive_id = ?2)",
                    params![&vehicle_key, id],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(StoreError::Query)?,
            None => false,
        };
        let hub_value = status == "success" && position.elevation == elevation;
        let coordinates_changed = position.latitude != latitude || position.longitude != longitude;
        let link_changed = drive != Some(old_drive);
        let clear = hub_value && (!drive_exists || link_changed || coordinates_changed);
        if clear {
            transaction
                .execute(
                    "UPDATE materialised_positions
                 SET position_json = json_set(position_json, '$.elevation', NULL)
                 WHERE vehicle_id = ?1 AND position_id = ?2",
                    params![&vehicle_key, position_id],
                )
                .map_err(StoreError::LifecycleWrite)?;
            position.elevation = None;
        }
        if !drive_exists {
            applied.recompute_drive_ids.push(old_drive);
        } else if link_changed {
            applied.recompute_drive_ids.push(old_drive);
            applied.recompute_drive_ids.extend(drive);
            requeue = position.elevation.is_none()
                && position.latitude.is_finite()
                && (-90.0..=90.0).contains(&position.latitude)
                && position.longitude.is_finite()
                && (-180.0..=180.0).contains(&position.longitude);
        } else if status == "success" {
            if !coordinates_changed && hub_value {
                return Ok(applied);
            }
            applied.recompute_drive_ids.push(old_drive);
            requeue = position.elevation.is_none();
        } else if position.elevation.is_none() {
            if !coordinates_changed {
                return Ok(applied);
            }
            requeue = true;
        }
        if requeue {
            requeue_rewritten_terrain(
                transaction,
                vehicle_id,
                position_id,
                drive,
                &position,
                now_ms,
            )?;
            applied.outcome = TerrainRewrite::Requeued;
            return Ok(applied);
        }
    } else {
        applied.recompute_drive_ids.push(old_drive);
    }
    transaction
        .execute(
            "DELETE FROM terrain_elevation_provenance WHERE vehicle_id = ?1 AND position_id = ?2",
            params![&vehicle_key, position_id],
        )
        .map_err(StoreError::LifecycleWrite)?;
    applied.outcome = TerrainRewrite::Removed;
    Ok(applied)
}

/// C1 R93, C9 R54, C9 R64: finalise written drives, then requested surviving drives.
fn finalise_position_drives_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    written: &[ProjectionDrive],
    inherited_ids: &[i64],
    terrain_ids: &[i64],
) -> Result<(), StoreError> {
    let mut requests = TerrainRecomputeRequests::default();
    requests.extend(
        inherited_ids
            .iter()
            .copied()
            .filter(|id| written.iter().any(|d| d.id == *id)),
    );
    requests.extend(terrain_ids.iter().copied());
    let vehicle_key = vehicle_id.to_string();
    for drive_id in requests.ids {
        let car: Option<i64> = transaction
            .query_row(
                "SELECT car_id FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = ?2",
                params![&vehicle_key, drive_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if car.is_none() {
            continue;
        }
        if written.iter().any(|drive| drive.id == drive_id) {
            recompute_terrain_drive_pair_with_change(transaction, vehicle_id, drive_id)?;
        } else {
            recompute_terrain_drive_with_change(transaction, vehicle_id, drive_id)?;
        }
    }
    Ok(())
}

// C9 ST76: thread-local observation counts calls, including unchanged totals.
#[cfg(test)]
thread_local! {
    static C9_RECOMPUTE_CALLS: std::cell::RefCell<Option<Vec<(i64, i64)>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn c9_record_recompute_call(
    transaction: &Transaction<'_>,
    vehicle_id: &str,
    drive_id: i64,
) -> Result<(), StoreError> {
    C9_RECOMPUTE_CALLS.with(|calls| {
        let mut calls = calls.borrow_mut();
        if let Some(calls) = calls.as_mut() {
            let positions = transaction.query_row(
                "SELECT count(*) FROM sync_mutations WHERE vehicle_id = ?1 AND entity = 'position'",
                [vehicle_id], |row| row.get(0),
            ).map_err(StoreError::Query)?;
            calls.push((drive_id, positions));
        }
        Ok(())
    })
}
