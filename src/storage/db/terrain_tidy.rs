// SPDX-License-Identifier: AGPL-3.0-only

/// C9 R55: a car or copied value excludes a drive even with stale success metadata.
const TERRAIN_TIDY_EXCLUDED_SQL: &str = "SELECT EXISTS(
    SELECT 1 FROM materialised_positions p INDEXED BY materialised_positions_drive
    WHERE p.vehicle_id = ?1 AND p.drive_id = ?2
      AND json_extract(p.position_json, '$.elevation') IS NOT NULL
      AND NOT EXISTS(SELECT 1 FROM terrain_elevation_provenance s
          WHERE s.vehicle_id = p.vehicle_id AND s.position_id = p.position_id
            AND s.status = 'success'
            AND json_type(p.position_json, '$.elevation') IN ('integer', 'real')
            AND s.elevation_m = json_extract(p.position_json, '$.elevation')))";

/// C9 R58: read the run inside the writer transaction before any chunk mutation.
fn terrain_tidy_run_in_transaction(connection: &Connection) -> Result<TerrainTidyRun, StoreError> {
    connection
        .query_row(
            "SELECT run, armed_at_ms FROM terrain_tidy_state
         WHERE step = 'earlier_build_values_v1'",
            [],
            |row| {
                Ok(TerrainTidyRun {
                    run: row.get(0)?,
                    armed_at_ms: row.get(1)?,
                })
            },
        )
        .map_err(StoreError::Query)
}

/// C9 R55: only the bounded provenance slice is retained in memory.
struct TerrainTidyRow {
    vehicle: Uuid,
    position: i64,
    latitude: f64,
    longitude: f64,
    elevation: Option<i64>,
    success: bool,
}

#[cfg(test)]
thread_local! {
    // C9 ST68, C9 ST74: reject the actual SQLite commit after all chunk writes.
    static TERRAIN_TIDY_REJECT_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl HubStore {
    /// C9 R55, C9 R56: tidy the next bounded provenance slice in one immediate transaction.
    /// Single-chunk callers and tests use this wrapper;
    /// the pass uses `apply_terrain_tidy_chunk_for_run`.
    pub fn apply_terrain_tidy_chunk(
        &self,
        rows: u32,
        now_ms: i64,
    ) -> Result<TerrainTidyApplied, StoreError> {
        let run = self.terrain_tidy_progress()?.identity();
        self.apply_terrain_tidy_chunk_for_run(rows, now_ms, run)
    }

    /// C9 R58: reject a chunk if its pass read an earlier run or armed time.
    pub fn apply_terrain_tidy_chunk_for_run(
        &self,
        rows: u32,
        now_ms: i64,
        run: TerrainTidyRun,
    ) -> Result<TerrainTidyApplied, StoreError> {
        if now_ms < 0 {
            return Err(StoreError::NegativeTimestamp("now_ms"));
        }
        let mut connection = self.open()?;
        #[cfg(test)]
        if TERRAIN_TIDY_REJECT_COMMIT.with(|flag| flag.replace(false)) {
            connection
                .commit_hook(Some(|| true))
                .map_err(StoreError::Configure)?;
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        admit_writable_catalogue(&transaction)?;
        if terrain_tidy_run_in_transaction(&transaction)? != run {
            return Ok(TerrainTidyApplied {
                rearmed: true,
                ..Default::default()
            });
        }
        let (mut resume_vehicle, mut resume_position, completed): (String, i64, Option<i64>) =
            transaction
                .query_row(
                    "SELECT resume_vehicle_id, resume_position_id, completed_at_ms
                 FROM terrain_tidy_state WHERE step = 'earlier_build_values_v1'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(StoreError::Query)?;
        if completed.is_some() || rows == 0 {
            return Ok(TerrainTidyApplied {
                complete: completed.is_some(),
                ..Default::default()
            });
        }
        let selected = {
            let mut statement = transaction.prepare(
                "SELECT vehicle_id, position_id, latitude, longitude, elevation_m, status = 'success'
                 FROM terrain_elevation_provenance
                 WHERE (vehicle_id, position_id) > (?1, ?2)
                 ORDER BY vehicle_id, position_id LIMIT ?3",
            ).map_err(StoreError::Query)?;
            let mut cursor = statement
                .query(params![resume_vehicle, resume_position, rows])
                .map_err(StoreError::Query)?;
            let mut selected = Vec::new();
            while let Some(row) = cursor.next().map_err(StoreError::Query)? {
                let key: String = row.get(0).map_err(StoreError::Query)?;
                selected.push(TerrainTidyRow {
                    vehicle: Uuid::parse_str(&key).map_err(|_| StoreError::InvalidVehicleId)?,
                    position: row.get(1).map_err(StoreError::Query)?,
                    latitude: row.get(2).map_err(StoreError::Query)?,
                    longitude: row.get(3).map_err(StoreError::Query)?,
                    elevation: row.get(4).map_err(StoreError::Query)?,
                    success: row.get(5).map_err(StoreError::Query)?,
                });
            }
            selected
        };
        // C9 R58 (b) 4: finish every boundary raise before acting on any selected row.
        let mut boundaries = HashMap::new();
        let mut live_vehicles = HashSet::new();
        let mut rearmed = false;
        for row in &selected {
            if boundaries.contains_key(&row.vehicle) {
                continue;
            }
            let exists: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM vehicles WHERE vehicle_id = ?1)",
                    [row.vehicle.to_string()],
                    |r| r.get(0),
                )
                .map_err(StoreError::Query)?;
            if exists {
                live_vehicles.insert(row.vehicle);
                let raised = terrain_after_import_boundary_raise_in_transaction(
                    &transaction,
                    row.vehicle,
                    now_ms,
                )?;
                rearmed |= raised == TerrainBoundaryRaise::RaisedAndRearmed;
            }
            boundaries.insert(
                row.vehicle,
                terrain_stored_boundary(&transaction, row.vehicle)?,
            );
        }
        if rearmed {
            transaction.commit().map_err(StoreError::LifecycleWrite)?;
            return Ok(TerrainTidyApplied {
                rearmed: true,
                ..Default::default()
            });
        }
        let mut applied = TerrainTidyApplied::default();
        let mut drive_states = HashMap::new();
        let mut affected = Vec::new();
        let mut seen_drives = HashSet::new();
        let mut changed_vehicles = Vec::new();
        let mut seen_vehicles = HashSet::new();
        for row in &selected {
            applied.examined += 1;
            resume_vehicle = row.vehicle.to_string();
            resume_position = row.position;
            if !row.success {
                continue;
            }
            let current = terrain_commit_position(&transaction, row.vehicle, row.position)?;
            let mut clear = false;
            let mut requeue = false;
            let mut drive_to_recompute = None;
            if let Some((drive, _, position)) = &current {
                let (exists, excluded) = if let Some(drive) = drive {
                    let key = (row.vehicle, *drive);
                    if let Some(state) = drive_states.get(&key) {
                        *state
                    } else {
                        let exists: bool = transaction
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM materialised_drives
                             WHERE vehicle_id = ?1 AND drive_id = ?2)",
                                params![resume_vehicle, drive],
                                |r| r.get(0),
                            )
                            .map_err(StoreError::Query)?;
                        let excluded: bool = exists
                            && transaction
                                .query_row(
                                    TERRAIN_TIDY_EXCLUDED_SQL,
                                    params![resume_vehicle, drive],
                                    |r| r.get(0),
                                )
                                .map_err(StoreError::Query)?;
                        drive_states.insert(key, (exists, excluded));
                        (exists, excluded)
                    }
                } else {
                    (false, false)
                };
                let boundary = boundaries[&row.vehicle];
                let imported = row.position <= boundary.max_position_id
                    && position.date_ms <= boundary.max_date_ms;
                let hub_value = position.elevation.is_some() && position.elevation == row.elevation;
                if hub_value && (imported || excluded) {
                    clear = true;
                    applied.cleared += 1;
                } else if position.elevation.is_some() && !hub_value
                    || position.elevation.is_none() && (imported || excluded || !exists)
                {
                    applied.removed += 1;
                } else if exists
                    && !imported
                    && !excluded
                    && (position.elevation.is_none()
                        || hub_value
                            && (position.latitude != row.latitude
                                || position.longitude != row.longitude))
                {
                    requeue = true;
                    clear = hub_value;
                    applied.requeued += 1;
                } else {
                    applied.kept += 1;
                    continue;
                }
                if exists {
                    drive_to_recompute = *drive;
                }
            } else {
                // C9 R55 T1: an orphan row is removed without a drive recompute.
                applied.removed += 1;
            }
            if clear {
                transaction
                    .execute(
                        "UPDATE materialised_positions
                     SET position_json = json_set(position_json, '$.elevation', NULL)
                     WHERE vehicle_id = ?1 AND position_id = ?2",
                        params![resume_vehicle, row.position],
                    )
                    .map_err(StoreError::LifecycleWrite)?;
                let payload: String = transaction
                    .query_row(
                        "SELECT position_json FROM materialised_positions
                     WHERE vehicle_id = ?1 AND position_id = ?2",
                        params![resume_vehicle, row.position],
                        |r| r.get(0),
                    )
                    .map_err(StoreError::Query)?;
                let car = current.as_ref().expect("cleared an existing position").1;
                record_sync_mutation_in_transaction(
                    &transaction,
                    row.vehicle,
                    "position",
                    row.position,
                    car,
                    "upsert",
                    &payload,
                )?;
            }
            if requeue {
                let (drive, _, position) = current.as_ref().expect("requeued an existing position");
                requeue_rewritten_terrain(
                    &transaction,
                    row.vehicle,
                    row.position,
                    *drive,
                    position,
                    now_ms,
                )?;
            } else {
                transaction
                    .execute(
                        "DELETE FROM terrain_elevation_provenance
                     WHERE vehicle_id = ?1 AND position_id = ?2",
                        params![resume_vehicle, row.position],
                    )
                    .map_err(StoreError::LifecycleWrite)?;
            }
            if let Some(drive) = drive_to_recompute
                && seen_drives.insert((row.vehicle, drive))
            {
                affected.push((row.vehicle, drive));
            }
            if live_vehicles.contains(&row.vehicle) && seen_vehicles.insert(row.vehicle) {
                changed_vehicles.push(row.vehicle);
            }
        }
        // C9 R39, C9 R55: one final recompute per affected drive, after position mutations.
        for (vehicle, drive) in affected {
            if recompute_terrain_drive_with_change(&transaction, vehicle, drive)? {
                applied.drives_changed += 1;
            }
        }
        for vehicle in changed_vehicles {
            mark_export_dirty_in_transaction(&transaction, vehicle)?;
        }
        applied.complete = applied.examined < rows;
        transaction
            .execute(
                "UPDATE terrain_tidy_state SET resume_vehicle_id = ?1, resume_position_id = ?2,
             examined = examined + ?3, cleared = cleared + ?4, requeued = requeued + ?5,
             removed = removed + ?6, kept = kept + ?7, drives_changed = drives_changed + ?8,
             completed_at_ms = ?9 WHERE step = 'earlier_build_values_v1'",
                params![
                    resume_vehicle,
                    resume_position,
                    applied.examined,
                    applied.cleared,
                    applied.requeued,
                    applied.removed,
                    applied.kept,
                    applied.drives_changed,
                    applied.complete.then_some(now_ms)
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(applied)
    }
}
