// SPDX-License-Identifier: AGPL-3.0-only

const TERRAIN_WINDOW_IDS_SQL: &str = "SELECT MAX(position_id) FROM (
    SELECT position_id FROM materialised_positions
    WHERE vehicle_id = ?1 AND position_id > ?2 ORDER BY position_id LIMIT ?3)";
pub(crate) const TERRAIN_WINDOW_REMAINS_SQL: &str = "SELECT EXISTS(
    SELECT 1 FROM materialised_positions WHERE vehicle_id = ?1 AND position_id > ?2)";
const TERRAIN_WINDOW_ROWS_SQL: &str = "SELECT p.drive_id, p.position_json,
    EXISTS(SELECT 1 FROM terrain_elevation_provenance t
           WHERE t.vehicle_id = p.vehicle_id AND t.position_id = p.position_id)
    FROM materialised_positions p
    WHERE p.vehicle_id = ?1 AND p.position_id > ?2 AND p.position_id <= ?3
    ORDER BY p.position_id";
const TERRAIN_DRIVE_ELIGIBLE_SQL: &str = "SELECT EXISTS(
    SELECT 1 FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = ?2)
    AND NOT EXISTS(
        SELECT 1 FROM materialised_positions p INDEXED BY materialised_positions_drive
        WHERE p.vehicle_id = ?1 AND p.drive_id = ?2
          AND json_extract(p.position_json, '$.elevation') IS NOT NULL
          AND NOT EXISTS(SELECT 1 FROM terrain_elevation_provenance t
              WHERE t.vehicle_id = p.vehicle_id AND t.position_id = p.position_id
                AND t.status = 'success'))";
const TERRAIN_DUE_RETRIES_SQL: &str = "SELECT t.vehicle_id, t.position_id, t.attempts,
    t.retry_after_ms, p.drive_id, p.position_json
    FROM terrain_elevation_provenance t
    LEFT JOIN materialised_positions p
      ON p.vehicle_id = t.vehicle_id AND p.position_id = t.position_id
    WHERE t.status = 'failed' AND t.retry_after_ms <= ?1
    ORDER BY t.retry_after_ms, t.vehicle_id, t.position_id LIMIT ?2";
const TERRAIN_WATERMARK_BOUNDARY_SQL: &str = "SELECT
    MAX(0, COALESCE(MAX(w.max_source_row_id), 0)),
    MAX(0, COALESCE(MAX(w.max_timestamp_ms), 0))
    FROM lifecycle_source_watermarks w JOIN sources s ON s.source_id = w.source_id
    WHERE w.vehicle_id = ?1 AND w.domain = 'positions' AND s.source_kind = ?2";

// C9 R58: walk only the raised provenance key range and stop at the first match.
const TERRAIN_BOUNDARY_SUCCESS_SQL: &str = "SELECT EXISTS(
    SELECT 1 FROM terrain_elevation_provenance t
        INDEXED BY sqlite_autoindex_terrain_elevation_provenance_1
    CROSS JOIN materialised_positions p
      ON p.vehicle_id = t.vehicle_id AND p.position_id = t.position_id
    WHERE t.vehicle_id = ?1 AND t.position_id <= ?2 AND t.status = 'success'
      AND CAST(json_extract(p.position_json, '$.date_ms') AS INTEGER) <= ?3
    ORDER BY t.position_id LIMIT 1)";

// C9 R24: each count stops after at most its per-vehicle cap of index entries.
const TERRAIN_BACKLOG_ROWS_SQL: &str = "SELECT COUNT(*) FROM (
    SELECT 1 FROM materialised_positions WHERE vehicle_id = ?1 AND position_id > ?2 LIMIT ?3)";
const TERRAIN_BACKLOG_DUE_SQL: &str = "SELECT COUNT(*) FROM (
    SELECT 1 FROM terrain_elevation_provenance INDEXED BY terrain_provenance_vehicle_retry
    WHERE vehicle_id = ?1 AND status = 'failed' AND retry_after_ms <= ?2 LIMIT ?3)";
const TERRAIN_BACKLOG_WAITING_SQL: &str = "SELECT COUNT(*) FROM (
    SELECT 1 FROM terrain_elevation_provenance INDEXED BY terrain_provenance_vehicle_retry
    WHERE vehicle_id = ?1 AND status = 'failed' AND retry_after_ms > ?2 LIMIT ?3)";

fn terrain_watermark_boundary(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<ImportBoundary, StoreError> {
    connection
        .query_row(
            TERRAIN_WATERMARK_BOUNDARY_SQL,
            params![vehicle_id.to_string(), IMPORT_SOURCE_KIND],
            |row| {
                Ok(ImportBoundary {
                    max_position_id: row.get(0)?,
                    max_date_ms: row.get(1)?,
                })
            },
        )
        .map_err(StoreError::Query)
}

fn terrain_stored_boundary(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<ImportBoundary, StoreError> {
    connection
        .query_row(
            "SELECT import_position_id_max, import_position_date_max_ms
        FROM terrain_enrichment_state WHERE vehicle_id = ?1",
            [vehicle_id.to_string()],
            |row| {
                Ok(ImportBoundary {
                    max_position_id: row.get(0)?,
                    max_date_ms: row.get(1)?,
                })
            },
        )
        .optional()
        .map(|value| value.unwrap_or_default())
        .map_err(StoreError::Query)
}

fn terrain_merged_boundary(stored: ImportBoundary, watermark: ImportBoundary) -> ImportBoundary {
    ImportBoundary {
        max_position_id: stored.max_position_id.max(watermark.max_position_id),
        max_date_ms: stored.max_date_ms.max(watermark.max_date_ms),
    }
}

/// C9 R58: the sole stored-boundary writer also re-arms affected tidy work.
fn raise_terrain_import_boundary_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    watermark: ImportBoundary,
    now_ms: i64,
) -> Result<TerrainBoundaryRaise, StoreError> {
    let stored = terrain_stored_boundary(transaction, vehicle_id)?;
    let raised = terrain_merged_boundary(stored, watermark);
    if raised == stored {
        return Ok(TerrainBoundaryRaise::Unchanged);
    }
    transaction.execute("INSERT INTO terrain_enrichment_state(
        vehicle_id, cursor_position_id, updated_at_ms, import_position_id_max, import_position_date_max_ms)
        VALUES (?1, 0, ?2, ?3, ?4) ON CONFLICT(vehicle_id) DO UPDATE SET
        import_position_id_max = excluded.import_position_id_max,
        import_position_date_max_ms = excluded.import_position_date_max_ms,
        updated_at_ms = excluded.updated_at_ms",
        params![vehicle_id.to_string(), now_ms, raised.max_position_id, raised.max_date_ms])
        .map_err(StoreError::Query)?;
    let affected: bool = transaction
        .query_row(
            TERRAIN_BOUNDARY_SUCCESS_SQL,
            params![
                vehicle_id.to_string(),
                raised.max_position_id,
                raised.max_date_ms
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    if !affected {
        return Ok(TerrainBoundaryRaise::Raised);
    }
    let changed = transaction
        .execute(
            "UPDATE terrain_tidy_state SET completed_at_ms = NULL,
        resume_vehicle_id = '', resume_position_id = 0, examined = 0, cleared = 0,
        requeued = 0, removed = 0, kept = 0, drives_changed = 0, run = run + 1, armed_at_ms = ?1
        WHERE step = 'earlier_build_values_v1'",
            [now_ms],
        )
        .map_err(StoreError::Query)?;
    if changed != 1 {
        // C9 R58 (c): a missing tidy marker is a store fault, not empty progress.
        return Err(StoreError::Query(rusqlite::Error::QueryReturnedNoRows));
    }
    Ok(TerrainBoundaryRaise::RaisedAndRearmed)
}

/// C9 R5 to C9 R7: cache one eligibility decision per drive within a selection.
fn terrain_qualifies(
    connection: &Connection,
    vehicle_id: Uuid,
    drive_id: Option<i64>,
    position: &ProjectionPosition,
    boundary: ImportBoundary,
    eligibility: &mut HashMap<(Uuid, i64), bool>,
) -> Result<bool, StoreError> {
    let Some(drive_id) = drive_id else {
        return Ok(false);
    };
    if position.elevation.is_some()
        || (position.id <= boundary.max_position_id && position.date_ms <= boundary.max_date_ms)
        || !position.latitude.is_finite()
        || !(-90.0..=90.0).contains(&position.latitude)
        || !position.longitude.is_finite()
        || !(-180.0..=180.0).contains(&position.longitude)
    {
        return Ok(false);
    }
    let key = (vehicle_id, drive_id);
    if let Some(eligible) = eligibility.get(&key) {
        return Ok(*eligible);
    }
    #[cfg(test)]
    if TERRAIN_SELECTION_READ_ACTIVE.with(|active| active.get()) {
        TERRAIN_ELIGIBILITY_READS.with(|count| count.set(count.get() + 1));
    }
    let eligible = connection
        .query_row(
            TERRAIN_DRIVE_ELIGIBLE_SQL,
            params![vehicle_id.to_string(), drive_id],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    eligibility.insert(key, eligible);
    Ok(eligible)
}

#[cfg(test)]
thread_local! {
    // C9 ST73: count the marker reader itself, independently of its callers.
    pub(crate) static TERRAIN_MARKER_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TERRAIN_SELECTION_READ_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // C9 R7: count selection eligibility evaluations, excluding commit rechecks.
    pub(crate) static TERRAIN_ELIGIBILITY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // C9 R8: allow a test writer to commit after the window upper bound is read.
    static TERRAIN_WINDOW_AFTER_UPPER: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

fn terrain_selection_qualifies(
    connection: &Connection,
    vehicle_id: Uuid,
    drive_id: Option<i64>,
    position: &ProjectionPosition,
    boundary: ImportBoundary,
    eligibility: &mut HashMap<(Uuid, i64), bool>,
) -> Result<bool, StoreError> {
    #[cfg(test)]
    {
        TERRAIN_SELECTION_READ_ACTIVE.with(|active| {
            let previous = active.replace(true);
            let result = terrain_qualifies(
                connection,
                vehicle_id,
                drive_id,
                position,
                boundary,
                eligibility,
            );
            active.set(previous);
            result
        })
    }
    #[cfg(not(test))]
    terrain_qualifies(
        connection,
        vehicle_id,
        drive_id,
        position,
        boundary,
        eligibility,
    )
}

impl HubStore {
    /// C9 R12: read vehicles in text-id order with their terrain selection state.
    pub fn terrain_vehicles(&self) -> Result<Vec<TerrainVehicle>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT v.vehicle_id, COALESCE(t.cursor_position_id, 0),
            COALESCE(t.import_position_id_max, 0), COALESCE(t.import_position_date_max_ms, 0)
            FROM vehicles v LEFT JOIN terrain_enrichment_state t ON t.vehicle_id = v.vehicle_id
            ORDER BY v.vehicle_id",
            )
            .map_err(StoreError::Query)?;
        let mut rows = statement.query([]).map_err(StoreError::Query)?;
        let mut vehicles = Vec::new();
        while let Some(row) = rows.next().map_err(StoreError::Query)? {
            let text: String = row.get(0).map_err(StoreError::Query)?;
            let vehicle_id = Uuid::parse_str(&text).map_err(|_| StoreError::InvalidVehicleId)?;
            vehicles.push(TerrainVehicle {
                vehicle_id,
                cursor_position_id: row.get(1).map_err(StoreError::Query)?,
                stored_boundary: ImportBoundary {
                    max_position_id: row.get(2).map_err(StoreError::Query)?,
                    max_date_ms: row.get(3).map_err(StoreError::Query)?,
                },
                watermark_boundary: terrain_watermark_boundary(&connection, vehicle_id)?,
            });
        }
        Ok(vehicles)
    }

    /// C9 R58: raise the stored import boundary and re-arm affected tidy work atomically.
    pub fn record_terrain_import_boundary(
        &self,
        vehicle_id: Uuid,
        boundary: ImportBoundary,
        now_ms: i64,
    ) -> Result<TerrainBoundaryRaise, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let result = raise_terrain_import_boundary_in_transaction(
            &transaction,
            vehicle_id,
            boundary,
            now_ms,
        )?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(result)
    }

    /// C9 R8: read a bounded forward window of one vehicle from one snapshot.
    pub fn terrain_forward_window(
        &self,
        vehicle: &TerrainVehicle,
        scan_rows: u32,
        limit: u32,
    ) -> Result<TerrainForwardWindow, StoreError> {
        self.terrain_forward_window_with_eligibility(vehicle, scan_rows, limit, &mut HashMap::new())
            .map(|(window, _)| window)
    }

    /// C9 R7, C9 R8: share a pass's drive decisions and report a cut window.
    pub(crate) fn terrain_forward_window_with_eligibility(
        &self,
        vehicle: &TerrainVehicle,
        scan_rows: u32,
        limit: u32,
        eligibility: &mut HashMap<(Uuid, i64), bool>,
    ) -> Result<(TerrainForwardWindow, bool), StoreError> {
        let mut connection = self.open_read_only_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(StoreError::Begin)?;
        let lower = vehicle.cursor_position_id;
        let upper: Option<i64> = transaction
            .query_row(
                TERRAIN_WINDOW_IDS_SQL,
                params![vehicle.vehicle_id.to_string(), lower, scan_rows],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let upper = upper.unwrap_or(lower);
        #[cfg(test)]
        TERRAIN_WINDOW_AFTER_UPPER.with(|slot| {
            let after_upper = slot.borrow_mut().take();
            if let Some(after_upper) = after_upper {
                after_upper();
            }
        });
        let mut result = TerrainForwardWindow {
            candidates: Vec::new(),
            examined_through: upper,
        };
        let boundary = terrain_merged_boundary(vehicle.stored_boundary, vehicle.watermark_boundary);
        let mut cut: bool = transaction
            .query_row(
                TERRAIN_WINDOW_REMAINS_SQL,
                params![vehicle.vehicle_id.to_string(), upper],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let mut statement = transaction
            .prepare(TERRAIN_WINDOW_ROWS_SQL)
            .map_err(StoreError::Query)?;
        let mut rows = statement
            .query(params![vehicle.vehicle_id.to_string(), lower, upper])
            .map_err(StoreError::Query)?;
        while let Some(row) = rows.next().map_err(StoreError::Query)? {
            if row.get::<_, bool>(2).map_err(StoreError::Query)? {
                continue;
            }
            let text: String = row.get(1).map_err(StoreError::Query)?;
            let position: ProjectionPosition =
                serde_json::from_str(&text).map_err(StoreError::DeserializeLifecycleRow)?;
            if !terrain_selection_qualifies(
                &transaction,
                vehicle.vehicle_id,
                row.get(0).map_err(StoreError::Query)?,
                &position,
                boundary,
                eligibility,
            )? {
                continue;
            }
            if result.candidates.len() == limit as usize {
                cut = true;
                // C9 R8: no returned candidate means a zero budget holds at L.
                result.examined_through = result
                    .candidates
                    .last()
                    .map_or(lower, |candidate| candidate.position.id);
                break;
            }
            result.candidates.push(TerrainCandidate {
                vehicle_id: vehicle.vehicle_id,
                position,
            });
        }
        Ok((result, cut))
    }

    /// C9 R10: read a bounded retry slice in due-time, vehicle and position order.
    pub fn terrain_due_retries(
        &self,
        now_ms: i64,
        limit: u32,
    ) -> Result<Vec<TerrainRetry>, StoreError> {
        self.terrain_due_retries_with_eligibility(now_ms, limit, &mut HashMap::new())
    }

    /// C9 R7, C9 R10: reuse the pass's drive decisions across both queues.
    pub(crate) fn terrain_due_retries_with_eligibility(
        &self,
        now_ms: i64,
        limit: u32,
        eligibility: &mut HashMap<(Uuid, i64), bool>,
    ) -> Result<Vec<TerrainRetry>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(TERRAIN_DUE_RETRIES_SQL)
            .map_err(StoreError::Query)?;
        let mut rows = statement
            .query(params![now_ms, limit])
            .map_err(StoreError::Query)?;
        let mut result = Vec::new();
        let mut boundaries = HashMap::new();
        while let Some(row) = rows.next().map_err(StoreError::Query)? {
            let text: String = row.get(0).map_err(StoreError::Query)?;
            let vehicle_id = Uuid::parse_str(&text).map_err(|_| StoreError::InvalidVehicleId)?;
            let boundary = if let Some(boundary) = boundaries.get(&vehicle_id) {
                *boundary
            } else {
                let boundary = terrain_merged_boundary(
                    terrain_stored_boundary(&connection, vehicle_id)?,
                    terrain_watermark_boundary(&connection, vehicle_id)?,
                );
                boundaries.insert(vehicle_id, boundary);
                boundary
            };
            let text: Option<String> = row.get(5).map_err(StoreError::Query)?;
            let candidate = if let Some(text) = text {
                let position: ProjectionPosition =
                    serde_json::from_str(&text).map_err(StoreError::DeserializeLifecycleRow)?;
                terrain_selection_qualifies(
                    &connection,
                    vehicle_id,
                    row.get(4).map_err(StoreError::Query)?,
                    &position,
                    boundary,
                    eligibility,
                )?
                .then_some(TerrainCandidate {
                    vehicle_id,
                    position,
                })
            } else {
                None
            };
            result.push(TerrainRetry {
                vehicle_id,
                position_id: row.get(1).map_err(StoreError::Query)?,
                attempts: row.get(2).map_err(StoreError::Query)?,
                retry_after_ms: row.get(3).map_err(StoreError::Query)?,
                candidate,
            });
        }
        Ok(result)
    }

    /// C9 R24: read the earliest failed terrain retry time, if any.
    pub fn terrain_next_retry_at(&self) -> Result<Option<i64>, StoreError> {
        self.open_read_only_connection()?
            .query_row(
                "SELECT MIN(retry_after_ms)
            FROM terrain_elevation_provenance WHERE status = 'failed'",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    /// C9 R24: sum separately capped per-vehicle terrain backlog counts.
    pub fn terrain_backlog(&self, now_ms: i64, cap: u32) -> Result<TerrainBacklog, StoreError> {
        let connection = self.open_read_only_connection()?;
        let mut result = TerrainBacklog::default();
        let mut statement = connection
            .prepare(
                "SELECT v.vehicle_id, COALESCE(t.cursor_position_id, 0)
             FROM vehicles v LEFT JOIN terrain_enrichment_state t ON t.vehicle_id = v.vehicle_id
             ORDER BY v.vehicle_id",
            )
            .map_err(StoreError::Query)?;
        let mut vehicles = statement.query([]).map_err(StoreError::Query)?;
        while let Some(vehicle) = vehicles.next().map_err(StoreError::Query)? {
            let text: String = vehicle.get(0).map_err(StoreError::Query)?;
            let cursor: i64 = vehicle.get(1).map_err(StoreError::Query)?;
            let unexamined: i64 = connection
                .query_row(
                    TERRAIN_BACKLOG_ROWS_SQL,
                    params![text, cursor, cap],
                    |row| row.get(0),
                )
                .map_err(StoreError::Query)?;
            let due: i64 = connection
                .query_row(TERRAIN_BACKLOG_DUE_SQL, params![text, now_ms, cap], |row| {
                    row.get(0)
                })
                .map_err(StoreError::Query)?;
            let waiting: i64 = connection
                .query_row(
                    TERRAIN_BACKLOG_WAITING_SQL,
                    params![text, now_ms, cap],
                    |row| row.get(0),
                )
                .map_err(StoreError::Query)?;
            result.rows_unexamined = result.rows_unexamined.saturating_add(
                u64::try_from(unexamined).map_err(|_| StoreError::InvalidStoredCount)?,
            );
            result.retries_due = result
                .retries_due
                .saturating_add(u64::try_from(due).map_err(|_| StoreError::InvalidStoredCount)?);
            result.retries_waiting = result.retries_waiting.saturating_add(
                u64::try_from(waiting).map_err(|_| StoreError::InvalidStoredCount)?,
            );
        }
        Ok(result)
    }

    /// C9 R56: read whether the terrain tidy marker is complete.
    pub fn terrain_tidy_complete(&self) -> Result<bool, StoreError> {
        Ok(self.terrain_tidy_progress()?.completed_at_ms.is_some())
    }

    /// C9 R58: read the current terrain tidy run and counts; a missing row is a fault.
    pub fn terrain_tidy_progress(&self) -> Result<TerrainTidyProgress, StoreError> {
        #[cfg(test)]
        TERRAIN_MARKER_READS.with(|count| count.set(count.get() + 1));
        self.open_read_only_connection()?
            .query_row(
                "SELECT run, examined, cleared, requeued,
            removed, kept, drives_changed, armed_at_ms, completed_at_ms FROM terrain_tidy_state
            WHERE step = 'earlier_build_values_v1'",
                [],
                |row| {
                    Ok(TerrainTidyProgress {
                        run: row.get(0)?,
                        examined: row.get(1)?,
                        cleared: row.get(2)?,
                        requeued: row.get(3)?,
                        removed: row.get(4)?,
                        kept: row.get(5)?,
                        drives_changed: row.get(6)?,
                        armed_at_ms: row.get(7)?,
                        completed_at_ms: row.get(8)?,
                    })
                },
            )
            .map_err(StoreError::Query)
    }
}
