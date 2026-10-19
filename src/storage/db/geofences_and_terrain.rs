// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    pub fn geofences(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Vec<crate::teslamate_projection::TeslaMateGeofence>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT source_geofence_id, name, latitude, longitude, radius_m,
                        billing_type, cost_per_unit, session_fee
                 FROM geofences WHERE vehicle_id = ?1 ORDER BY source_geofence_id",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map(params![vehicle_id.to_string()], |row| {
                let billing_type = row
                    .get::<_, Option<String>>(5)?
                    .and_then(|value| value.parse().ok());
                Ok(crate::teslamate_projection::TeslaMateGeofence {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    latitude: Some(row.get(2)?),
                    longitude: Some(row.get(3)?),
                    radius_m: Some(row.get(4)?),
                    billing_type,
                    cost_per_unit: row.get(6)?,
                    session_fee: row.get(7)?,
                })
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)
    }

    pub fn save_geofence(
        &self,
        vehicle_id: Uuid,
        source_geofence_id: Option<i64>,
        mut geofence: crate::teslamate_projection::TeslaMateGeofence,
    ) -> Result<crate::teslamate_projection::TeslaMateGeofence, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let name = geofence.name.trim().to_owned();
        let Some((latitude, longitude, radius_m)) = geofence.valid_geometry() else {
            return Err(StoreError::InvalidGeofence);
        };
        if radius_m <= 0.0
            || name.is_empty()
            || name.len() > 256
            || name.chars().any(char::is_control)
            || geofence.billing_type.is_none()
            || geofence
                .cost_per_unit
                .is_some_and(|value| !value.is_finite() || value < 0.0)
            || geofence
                .session_fee
                .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            return Err(StoreError::InvalidGeofence);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let vehicle_exists = transaction
            .query_row(
                "SELECT 1 FROM vehicles WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(StoreError::Query)?
            .is_some();
        if !vehicle_exists {
            return Err(StoreError::UnknownVehicle(vehicle_id));
        }
        let source_geofence_id = match source_geofence_id {
            Some(id) if id > 0 => id,
            Some(_) => return Err(StoreError::InvalidGeofence),
            None => transaction
                .query_row(
                    "SELECT COALESCE(MAX(source_geofence_id), 0) + 1
                     FROM geofences WHERE vehicle_id = ?1",
                    params![vehicle_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(StoreError::Query)?,
        };
        transaction
            .execute(
                "INSERT INTO geofences(
                    vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                    billing_type, cost_per_unit, session_fee
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(vehicle_id, source_geofence_id) DO UPDATE SET
                    name = excluded.name,
                    latitude = excluded.latitude,
                    longitude = excluded.longitude,
                    radius_m = excluded.radius_m,
                    billing_type = excluded.billing_type,
                    cost_per_unit = excluded.cost_per_unit,
                    session_fee = excluded.session_fee",
                params![
                    vehicle_id.to_string(),
                    source_geofence_id,
                    &name,
                    latitude,
                    longitude,
                    radius_m,
                    geofence
                        .billing_type
                        .map(crate::hub_pack::GeofenceBillingType::as_str),
                    geofence.cost_per_unit,
                    geofence.session_fee,
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        relabel_materialised_locations_in_transaction(&transaction, vehicle_id)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        geofence.id = source_geofence_id;
        geofence.name = name;
        Ok(geofence)
    }

    pub fn delete_geofence(
        &self,
        vehicle_id: Uuid,
        source_geofence_id: i64,
    ) -> Result<(), StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if source_geofence_id <= 0 {
            return Err(StoreError::InvalidGeofence);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let deleted = transaction
            .execute(
                "DELETE FROM geofences WHERE vehicle_id = ?1 AND source_geofence_id = ?2",
                params![vehicle_id.to_string(), source_geofence_id],
            )
            .map_err(StoreError::LifecycleWrite)?;
        if deleted == 0 {
            return Err(StoreError::UnknownGeofence(source_geofence_id));
        }
        relabel_materialised_locations_in_transaction(&transaction, vehicle_id)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }

    /// Fill the missing costs of the Hub charges that started inside one fence,
    /// when the owner asks for it (C2 R49 to R51). Returns the number of rows
    /// changed.
    ///
    /// Everything runs in one `IMMEDIATE` transaction. The errors come first
    /// and in this order: a nil vehicle id, a fence id below one, a fence the
    /// vehicle does not have. Then the walk takes the vehicle's charge rows in
    /// charge-id order, one bounded page at a time, and fills a row iff all of
    /// these hold:
    ///
    /// - its cost is unknown (a known cost, computed, imported or entered by
    ///   hand, is never replaced: R47, R48);
    /// - it is a Hub row, not an imported one (R5, R51);
    /// - its start coordinates are known and the one geofence rule over all of
    ///   the vehicle's fences picks this very fence, so a smaller overlapping
    ///   fence keeps its sessions as it does at close (C1 R57);
    /// - the fence's tariff is complete and valid (R40);
    /// - the cost over that tariff, with the session fee waived when the row
    ///   continues its plug-in (R44a), is known (R44).
    ///
    /// A filled row takes the cost and the tariff copy together, so it never
    /// disagrees with its own tariff fields. The fence's current tariff is
    /// used because the owner has just saved it and asked for it to price the
    /// missing costs; the car's current `free_supercharging` is used because
    /// the historic value is not recorded. Each change is one `upsert` of a
    /// `charge` sync mutation, and the export is marked dirty once. A second
    /// identical call changes nothing and returns 0.
    pub fn recalculate_missing_charge_costs(
        &self,
        vehicle_id: Uuid,
        source_geofence_id: i64,
    ) -> Result<u64, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if source_geofence_id <= 0 {
            return Err(StoreError::InvalidGeofence);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let target = geofence_fence_by_id(&transaction, vehicle_id, source_geofence_id)?
            .ok_or(StoreError::UnknownGeofence(source_geofence_id))?;
        let tariff = crate::lifecycle::ChargeTariff::of_fence(&target);
        let changed = match tariff {
            Some(tariff) => {
                fill_missing_charge_costs_in_transaction(&transaction, vehicle_id, &target, tariff)?
            }
            // A fence without a usable tariff prices nothing (R40).
            None => 0,
        };
        if changed > 0 {
            mark_export_dirty_in_transaction(&transaction, vehicle_id)?;
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        // R59: the vehicle id, the count and the outcome only; never the fence
        // name or any coordinate (R7).
        tracing::info!(
            vehicle_id = %vehicle_id,
            filled = changed,
            "missing charge costs filled"
        );
        Ok(changed)
    }

    /// Preserve imported geofence labels and geometry as a durable, append-only
    /// catalog. Invalid geometry is skipped so unrelated history can proceed.
    pub fn upsert_geofences(
        &self,
        vehicle_id: Uuid,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    ) -> Result<usize, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let mut inserted = 0;
        for geofence in geofences {
            let Some((latitude, longitude, radius_m)) = geofence.valid_geometry() else {
                continue;
            };
            if geofence.name.trim().is_empty() || geofence.name.len() > 256 {
                continue;
            }
            inserted += transaction
                .execute(
                    "INSERT INTO geofences(
                        vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                        billing_type, cost_per_unit, session_fee
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(vehicle_id, source_geofence_id) DO NOTHING",
                    params![
                        vehicle_id.to_string(),
                        geofence.id,
                        geofence.name.trim(),
                        latitude,
                        longitude,
                        radius_m,
                        geofence
                            .billing_type
                            .map(crate::hub_pack::GeofenceBillingType::as_str),
                        geofence.cost_per_unit,
                        geofence.session_fee,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "UPDATE geofences SET
                        name = ?3, latitude = ?4, longitude = ?5, radius_m = ?6,
                        billing_type = COALESCE(?7, billing_type),
                        cost_per_unit = COALESCE(?8, cost_per_unit),
                        session_fee = COALESCE(?9, session_fee)
                     WHERE vehicle_id = ?1 AND source_geofence_id = ?2",
                    params![
                        vehicle_id.to_string(),
                        geofence.id,
                        geofence.name.trim(),
                        latitude,
                        longitude,
                        radius_m,
                        geofence
                            .billing_type
                            .map(crate::hub_pack::GeofenceBillingType::as_str),
                        geofence.cost_per_unit,
                        geofence.session_fee,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(inserted)
    }

    /// Persist open-session state and append newly completed history rows.
    pub fn commit_lifecycle_delta(&self, commit: &LifecycleCommit<'_>) -> Result<(), StoreError> {
        if commit.vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if commit.car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }
        if commit.last_observation_id < 0 {
            return Err(StoreError::InvalidLifecycleCursor);
        }
        validate_timestamp("lifecycle updated_at_ms", commit.updated_at_ms)?;
        if commit.open_session_json.len() < 2 || commit.open_session_json.len() > 65_536 {
            return Err(StoreError::InvalidLifecycleSession);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        Self::commit_lifecycle_delta_in_transaction(&transaction, commit)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }

    /// [`Self::commit_lifecycle_delta`] with the page size of the stored-children
    /// reads given and the page statistics returned (C1 R88, tests).
    #[cfg(test)]
    pub(crate) fn commit_lifecycle_delta_with_page_rows(
        &self,
        commit: &LifecycleCommit<'_>,
        page_rows: usize,
    ) -> Result<(usize, usize, ClosePageStats), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let mut stats = ClosePageStats::default();
        let (positions, samples) =
            Self::commit_lifecycle_delta_paged(&transaction, commit, page_rows, &mut stats)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok((positions, samples, stats))
    }

    /// Add the ids of one parent's stored open children to `ids`, reading ids
    /// only, `page_rows` at a time (C1 R88, the collector's count).
    pub(crate) fn open_child_ids_paged(
        &self,
        vehicle_id: Uuid,
        kind: OpenChildKind,
        parent_id: i64,
        page_rows: usize,
        ids: &mut std::collections::HashSet<i64>,
        stats: &mut ClosePageStats,
    ) -> Result<(), StoreError> {
        let connection = self.open()?;
        collect_open_child_ids(
            &connection,
            &vehicle_id.to_string(),
            kind,
            parent_id,
            page_rows,
            ids,
            stats,
        )
    }

    fn maybe_stream_fault(&self, point: StreamFaultPoint) -> Result<(), StoreError> {
        #[cfg(test)]
        {
            let mut fault = self.stream_fault.lock().expect("stream fault lock");
            if fault.as_ref().is_some_and(|value| *value == point) {
                *fault = None;
                return Err(StoreError::InjectedStreamFault(point.label()));
            }
        }
        #[cfg(not(test))]
        let _ = point;
        Ok(())
    }

    #[cfg(test)]
    pub fn inject_stream_fault(&self, point: StreamFaultPoint) {
        *self.stream_fault.lock().expect("stream fault lock") = Some(point);
    }

    #[cfg(test)]
    pub(crate) fn inject_projection_state_detach_fault(&self) {
        *self
            .projection_state_detach_fault
            .lock()
            .expect("projection-state detach fault lock") = true;
    }

    /// Write one lifecycle commit and return how many positions and charge
    /// samples it materialised, counting the open children of closed and
    /// discarded sessions it pulled from `lifecycle_open_rows` (C1 R11, R48).
    pub(crate) fn commit_lifecycle_delta_in_transaction(
        transaction: &Transaction<'_>,
        commit: &LifecycleCommit<'_>,
    ) -> Result<(usize, usize), StoreError> {
        let mut stats = ClosePageStats::default();
        Self::commit_lifecycle_delta_paged(
            transaction,
            commit,
            crate::lifecycle::K_CLOSE_PAGE_ROWS,
            &mut stats,
        )
    }

    /// The commit of [`Self::commit_lifecycle_delta_in_transaction`] with the
    /// page size of the stored-children reads as an argument (C1 R88).
    /// Production passes `K_CLOSE_PAGE_ROWS`.
    fn commit_lifecycle_delta_paged(
        transaction: &Transaction<'_>,
        commit: &LifecycleCommit<'_>,
        page_rows: usize,
        stats: &mut ClosePageStats,
    ) -> Result<(usize, usize), StoreError> {
        let mut delta = commit.delta.clone();
        let session = crate::lifecycle::OpenSessionState::decode(commit.open_session_json)
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        let lifecycle_source_id: String = transaction
            .query_row(
                "SELECT source_id FROM vehicles WHERE vehicle_id = ?1",
                params![commit.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LifecycleWrite)?;
        let vehicle_key = commit.vehicle_id.to_string();
        for position in &delta.open_drive_positions {
            insert_open_row(
                transaction,
                &lifecycle_source_id,
                "positions",
                position.id,
                &vehicle_key,
                commit.car_id,
                "position",
                position.drive_id,
                position,
            )?;
        }
        for sample in &delta.open_charge_samples {
            insert_open_row(
                transaction,
                &lifecycle_source_id,
                "charges",
                sample.id,
                &vehicle_key,
                commit.car_id,
                "charge_sample",
                Some(sample.charge_process_id),
                sample,
            )?;
        }
        // A drive or charge that closes without a full in-memory child buffer
        // has its stored children materialised after the closing rows, a page at
        // a time (C1 R88), never all at once.
        //
        // Child placement (C1 R48, R48a): a closed drive keeps the children
        // dated at or before its end; those dated later (the stationary tail a
        // lack-of-evidence close cuts off) and every child of a discarded drive
        // are materialised as standalone positions (`drive_id` NULL) instead of
        // being kept under the drive or deleted. The Hub stores positions
        // losslessly and the observations behind them are pruned after commit.
        //
        // The in-delta rows are written first, in delta order; the set of
        // placed ids starts with their ids and receives each stored child's id
        // as it is placed, so a stored child already in the delta is skipped.
        let mut placed_position_ids = delta
            .positions
            .iter()
            .map(|position| position.id)
            .collect::<std::collections::HashSet<_>>();
        for position in &mut delta.positions {
            if let Some(drive_id) = position.drive_id
                && (delta.discarded_drive_ids.contains(&drive_id)
                    || delta
                        .drives
                        .iter()
                        .any(|drive| drive.id == drive_id && position.date_ms > drive.end_date_ms))
            {
                position.drive_id = None;
            }
        }
        let mut placed_sample_ids = delta
            .charge_samples
            .iter()
            .map(|sample| sample.id)
            .collect::<std::collections::HashSet<_>>();
        let fences = load_geofence_fences(transaction, commit.vehicle_id)?;
        // Only closed drives carry labels, so a delta without drives needs no
        // matching pass (C1 R59).
        //
        // The unit emits drives without labels, so for live rows every label is
        // set here. A row that already carries a label (an imported or
        // externally supplied row) keeps it: relabelling after a fence change
        // is the job of `relabel_materialised_locations_in_transaction`.
        if !delta.drives.is_empty() {
            let existing = delta
                .drives
                .iter()
                .map(|drive| (drive.start_geofence.clone(), drive.end_geofence.clone()))
                .collect::<Vec<_>>();
            crate::lifecycle::apply_geofence_labels(&mut delta, &fences);
            for (drive, (start, end)) in delta.drives.iter_mut().zip(existing) {
                if start.is_some() {
                    drive.start_geofence = start;
                }
                if end.is_some() {
                    drive.end_geofence = end;
                }
            }
        }
        if let Some(patch) = session.car_metadata.as_ref() {
            let existing_json: Option<String> = transaction
                .query_row(
                    "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
                    params![commit.vehicle_id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::LifecycleWrite)?;
            let existing = existing_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(StoreError::DeserializeLifecycleRow)?;
            let (fallback_name, fallback_vin): (Option<String>, Option<String>) = transaction
                .query_row(
                    "SELECT display_name, vin FROM vehicles WHERE vehicle_id = ?1",
                    params![commit.vehicle_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(StoreError::LifecycleWrite)?;
            let car = patch.into_car(
                commit.car_id,
                existing.as_ref(),
                fallback_name,
                fallback_vin,
            );
            let car_json =
                serde_json::to_string(&car).map_err(StoreError::SerializeLifecycleRow)?;
            let car_name = car.name.clone();
            let car_vin = car.vin.clone();
            transaction
                .execute(
                    "UPDATE vehicles SET display_name = COALESCE(?1, display_name), \
                         vin = COALESCE(?2, vin), last_seen_at_ms = MAX(last_seen_at_ms, ?3) \
                     WHERE vehicle_id = ?4",
                    params![
                        car_name,
                        car_vin,
                        commit.updated_at_ms,
                        commit.vehicle_id.to_string()
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            if existing.as_ref() != Some(&car) {
                transaction
                    .execute(
                        "INSERT INTO materialised_cars(vehicle_id, car_id, car_json) \
                         VALUES (?1, ?2, ?3) \
                         ON CONFLICT(vehicle_id) DO UPDATE SET \
                             car_id = excluded.car_id, car_json = excluded.car_json",
                        params![commit.vehicle_id.to_string(), car.id, car_json],
                    )
                    .map_err(StoreError::LifecycleWrite)?;
                record_sync_mutation_in_transaction(
                    transaction,
                    commit.vehicle_id,
                    "car",
                    car.id,
                    commit.car_id,
                    "upsert",
                    &car_json,
                )?;
            }
        }

        transaction
            .execute(
                "INSERT INTO vehicle_lifecycle_state(
                    vehicle_id, car_id, last_observation_id, open_session_json,
                    quarantined, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    car_id = excluded.car_id,
                    last_observation_id = excluded.last_observation_id,
                    open_session_json = excluded.open_session_json,
                    quarantined = excluded.quarantined,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    commit.vehicle_id.to_string(),
                    commit.car_id,
                    commit.last_observation_id,
                    commit.open_session_json,
                    i64::from(commit.quarantined),
                    commit.updated_at_ms,
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        mark_export_dirty_in_transaction(transaction, commit.vehicle_id)?;

        for drive in &delta.drives {
            let drive_json =
                serde_json::to_string(drive).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_drives(
                        vehicle_id, drive_id, car_id, drive_json,
                        inside_temp_avg, power_max, power_min,
                        start_ideal_range_km, end_ideal_range_km, ascent, descent
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                    ON CONFLICT(vehicle_id, drive_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        drive_json = excluded.drive_json,
                        inside_temp_avg = excluded.inside_temp_avg,
                        power_max = excluded.power_max,
                        power_min = excluded.power_min,
                        start_ideal_range_km = excluded.start_ideal_range_km,
                        end_ideal_range_km = excluded.end_ideal_range_km,
                        ascent = excluded.ascent,
                        descent = excluded.descent",
                    params![
                        commit.vehicle_id.to_string(),
                        drive.id,
                        commit.car_id,
                        drive_json,
                        drive.inside_temp_avg,
                        drive.power_max,
                        drive.power_min,
                        drive.start_ideal_range_km,
                        drive.end_ideal_range_km,
                        drive.ascent,
                        drive.descent
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "drive",
                drive.id,
                commit.car_id,
                "upsert",
                &drive_json,
            )?;
        }
        let mut positions_written = 0usize;
        for position in &delta.positions {
            Self::write_materialised_position(transaction, commit, position)?;
            positions_written += 1;
        }
        for drive in &delta.drives {
            for_each_open_child_page(
                transaction,
                &vehicle_key,
                OpenChildKind::Position,
                drive.id,
                page_rows,
                stats,
                |_, json| {
                    let mut position: crate::hub_pack::ProjectionPosition =
                        serde_json::from_str(json).map_err(StoreError::DeserializeLifecycleRow)?;
                    if !placed_position_ids.insert(position.id) {
                        return Ok(());
                    }
                    if position.date_ms > drive.end_date_ms {
                        position.drive_id = None;
                    }
                    Self::write_materialised_position(transaction, commit, &position)?;
                    positions_written += 1;
                    Ok(())
                },
            )?;
        }
        for drive_id in &delta.discarded_drive_ids {
            for_each_open_child_page(
                transaction,
                &vehicle_key,
                OpenChildKind::Position,
                *drive_id,
                page_rows,
                stats,
                |_, json| {
                    let mut position: crate::hub_pack::ProjectionPosition =
                        serde_json::from_str(json).map_err(StoreError::DeserializeLifecycleRow)?;
                    if !placed_position_ids.insert(position.id) {
                        return Ok(());
                    }
                    position.drive_id = None;
                    Self::write_materialised_position(transaction, commit, &position)?;
                    positions_written += 1;
                    Ok(())
                },
            )?;
        }
        close_charges_in_transaction(transaction, commit)?;
        // Samples follow the same order after the close step: the in-delta
        // rows, then each closed charge's stored samples in `delta.charges`
        // order and id order, a page at a time (C1 R88).
        let mut samples_written = 0usize;
        for sample in &delta.charge_samples {
            Self::write_materialised_charge_sample(transaction, commit, sample)?;
            samples_written += 1;
        }
        for charge in &delta.charges {
            for_each_open_child_page(
                transaction,
                &vehicle_key,
                OpenChildKind::ChargeSample,
                charge.id,
                page_rows,
                stats,
                |_, json| {
                    let sample = parse_open_charge_sample(json)?;
                    if !placed_sample_ids.insert(sample.id) {
                        return Ok(());
                    }
                    Self::write_materialised_charge_sample(transaction, commit, &sample)?;
                    samples_written += 1;
                    Ok(())
                },
            )?;
        }
        // R67: once per commit that closes at least one charge, after the
        // closing rows and their samples are materialised, because the
        // session figure reads the samples; the commit's own session counts.
        if !delta.charges.is_empty() {
            recompute_car_efficiency(transaction, commit.vehicle_id, commit.car_id)?;
        }

        for drive in &delta.drives {
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'position'
                       AND parent_source_row_id = ?2",
                    params![vehicle_key, drive.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'drive' AND source_row_id = ?2",
                    params![vehicle_key, drive.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        for drive_id in &delta.discarded_drive_ids {
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'position'
                       AND parent_source_row_id = ?2",
                    params![vehicle_key, drive_id],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'drive' AND source_row_id = ?2",
                    params![vehicle_key, drive_id],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        for charge in &delta.charges {
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'charge_sample'
                       AND parent_source_row_id = ?2",
                    params![vehicle_key, charge.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'charge' AND source_row_id = ?2",
                    params![vehicle_key, charge.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        for state in &delta.states {
            if state.end_date_ms.is_some() {
                transaction
                    .execute(
                        "DELETE FROM lifecycle_open_rows
                         WHERE vehicle_id = ?1 AND domain = 'state' AND source_row_id = ?2",
                        params![vehicle_key, state.id],
                    )
                    .map_err(StoreError::LifecycleWrite)?;
            }
        }
        for state in &delta.states {
            let state_json =
                serde_json::to_string(state).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_states(
                        vehicle_id, state_id, car_id, state_json
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(vehicle_id, state_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        state_json = excluded.state_json",
                    params![
                        commit.vehicle_id.to_string(),
                        state.id,
                        commit.car_id,
                        state_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "state",
                state.id,
                commit.car_id,
                "upsert",
                &state_json,
            )?;
        }
        for update in &delta.updates {
            let update_json =
                serde_json::to_string(update).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_updates(
                        vehicle_id, update_id, car_id, update_json
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(vehicle_id, update_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        update_json = excluded.update_json",
                    params![
                        commit.vehicle_id.to_string(),
                        update.id,
                        commit.car_id,
                        update_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "update",
                update.id,
                commit.car_id,
                "upsert",
                &update_json,
            )?;
        }

        enqueue_address_jobs(transaction, commit.vehicle_id, &delta)?;
        prune_processed_observations(transaction, commit.vehicle_id, commit.last_observation_id)?;

        Ok((positions_written, samples_written))
    }

    /// Write one materialised position and its sync mutation (C1 R11, R48).
    fn write_materialised_position(
        transaction: &Transaction<'_>,
        commit: &LifecycleCommit<'_>,
        position: &crate::hub_pack::ProjectionPosition,
    ) -> Result<(), StoreError> {
        let position_json =
            serde_json::to_string(position).map_err(StoreError::SerializeLifecycleRow)?;
        transaction
            .execute(
                "INSERT INTO materialised_positions(
                    vehicle_id, position_id, drive_id, car_id, position_json,
                    speed, power, est_battery_range_km, fan_status,
                    driver_temp_setting, passenger_temp_setting,
                    is_climate_on, is_rear_defroster_on, is_front_defroster_on,
                    battery_heater, battery_heater_on, battery_heater_no_power,
                    tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                           ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                 ON CONFLICT(vehicle_id, position_id) DO UPDATE SET
                    drive_id = excluded.drive_id,
                    car_id = excluded.car_id,
                    position_json = excluded.position_json,
                    speed = excluded.speed,
                    power = excluded.power,
                    est_battery_range_km = excluded.est_battery_range_km,
                    fan_status = excluded.fan_status,
                    driver_temp_setting = excluded.driver_temp_setting,
                    passenger_temp_setting = excluded.passenger_temp_setting,
                    is_climate_on = excluded.is_climate_on,
                    is_rear_defroster_on = excluded.is_rear_defroster_on,
                    is_front_defroster_on = excluded.is_front_defroster_on,
                    battery_heater = excluded.battery_heater,
                    battery_heater_on = excluded.battery_heater_on,
                    battery_heater_no_power = excluded.battery_heater_no_power,
                    tpms_pressure_fl = excluded.tpms_pressure_fl,
                    tpms_pressure_fr = excluded.tpms_pressure_fr,
                    tpms_pressure_rl = excluded.tpms_pressure_rl,
                    tpms_pressure_rr = excluded.tpms_pressure_rr",
                params![
                    commit.vehicle_id.to_string(),
                    position.id,
                    position.drive_id,
                    commit.car_id,
                    position_json,
                    position.speed,
                    position.power,
                    position.est_battery_range_km,
                    position.fan_status,
                    position.driver_temp_setting,
                    position.passenger_temp_setting,
                    position.is_climate_on.map(i64::from),
                    position.is_rear_defroster_on.map(i64::from),
                    position.is_front_defroster_on.map(i64::from),
                    position.battery_heater.map(i64::from),
                    position.battery_heater_on.map(i64::from),
                    position.battery_heater_no_power.map(i64::from),
                    position.tpms_pressure_fl,
                    position.tpms_pressure_fr,
                    position.tpms_pressure_rl,
                    position.tpms_pressure_rr,
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        record_sync_mutation_in_transaction(
            transaction,
            commit.vehicle_id,
            "position",
            position.id,
            commit.car_id,
            "upsert",
            &position_json,
        )?;
        Ok(())
    }

    /// Write one materialised charge sample and its sync mutation (C1 R11).
    fn write_materialised_charge_sample(
        transaction: &Transaction<'_>,
        commit: &LifecycleCommit<'_>,
        sample: &crate::hub_pack::ProjectionChargeSample,
    ) -> Result<(), StoreError> {
        let sample_json =
            serde_json::to_string(sample).map_err(StoreError::SerializeLifecycleRow)?;
        transaction
            .execute(
                "INSERT INTO materialised_charge_samples(
                    vehicle_id, sample_id, charge_id, sample_json
                ) VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(vehicle_id, sample_id) DO UPDATE SET
                    charge_id = excluded.charge_id,
                    sample_json = excluded.sample_json",
                params![
                    commit.vehicle_id.to_string(),
                    sample.id,
                    sample.charge_process_id,
                    sample_json
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        record_sync_mutation_in_transaction(
            transaction,
            commit.vehicle_id,
            "charge_sample",
            sample.id,
            commit.car_id,
            "upsert",
            &sample_json,
        )?;
        Ok(())
    }

    /// Load completed history used when publishing a phone snapshot.
    pub fn materialised_history(
        &self,
        vehicle_id: Uuid,
    ) -> Result<MaterialisedHistory, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let vehicle_key = vehicle_id.to_string();

        let drives = load_json_rows(
            &connection,
            "SELECT drive_json FROM materialised_drives WHERE vehicle_id = ?1 ORDER BY drive_id ASC",
            &vehicle_key,
        )?;
        let positions = load_json_rows(
            &connection,
            "SELECT position_json FROM materialised_positions WHERE vehicle_id = ?1 ORDER BY position_id ASC",
            &vehicle_key,
        )?;
        let charges = load_json_rows(
            &connection,
            "SELECT charge_json FROM materialised_charges WHERE vehicle_id = ?1 ORDER BY charge_id ASC",
            &vehicle_key,
        )?;
        let charge_samples = load_json_rows(
            &connection,
            "SELECT sample_json FROM materialised_charge_samples WHERE vehicle_id = ?1 ORDER BY sample_id ASC",
            &vehicle_key,
        )?;
        let states = load_json_rows(
            &connection,
            "SELECT state_json FROM materialised_states WHERE vehicle_id = ?1 ORDER BY state_id ASC",
            &vehicle_key,
        )?;
        let updates = load_json_rows(
            &connection,
            "SELECT update_json FROM materialised_updates WHERE vehicle_id = ?1 ORDER BY update_id ASC",
            &vehicle_key,
        )?;
        let car = connection
            .query_row(
                "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
                params![vehicle_key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Query)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(StoreError::DeserializeLifecycleRow)?;
        Ok(MaterialisedHistory {
            car,
            drives,
            positions,
            charges,
            charge_samples,
            states,
            updates,
        })
    }

    pub fn terrain_candidates(
        &self,
        now_ms: i64,
        limit: u32,
    ) -> Result<Vec<TerrainCandidate>, StoreError> {
        let limit = i64::from(limit.min(1_000));
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT p.vehicle_id, p.position_json
                 FROM materialised_positions p
                 JOIN materialised_drives d
                   ON d.vehicle_id = p.vehicle_id AND d.drive_id = p.drive_id
                 LEFT JOIN terrain_elevation_provenance e
                   ON e.vehicle_id = p.vehicle_id AND e.position_id = p.position_id
                 LEFT JOIN terrain_enrichment_state c
                   ON c.vehicle_id = p.vehicle_id
                 WHERE json_extract(p.position_json, '$.elevation') IS NULL
                   AND (e.status IS NULL OR
                        (e.status = 'failed' AND COALESCE(e.retry_after_ms, 0) <= ?1))
                   AND (p.position_id > COALESCE(c.cursor_position_id, 0)
                        OR e.status = 'failed')
                   AND NOT EXISTS (
                       SELECT 1 FROM materialised_positions streamed
                       WHERE streamed.vehicle_id = p.vehicle_id
                         AND streamed.drive_id = p.drive_id
                         AND json_extract(streamed.position_json, '$.odometer') IS NOT NULL
                         AND json_extract(
                               streamed.position_json,
                               '$.ideal_battery_range_km'
                             ) IS NULL
                   )
                 ORDER BY p.vehicle_id ASC, p.position_id ASC
                 LIMIT ?2",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![now_ms, limit], |row| {
                let vehicle_id: String = row.get(0)?;
                let position_json: String = row.get(1)?;
                Ok((vehicle_id, position_json))
            })
            .map_err(StoreError::Query)?;
        rows.map(|row| {
            let (vehicle_id, position_json) = row.map_err(StoreError::Query)?;
            let vehicle_id =
                Uuid::parse_str(&vehicle_id).map_err(|_| StoreError::InvalidVehicleId)?;
            let position = serde_json::from_str(&position_json)
                .map_err(StoreError::DeserializeLifecycleRow)?;
            Ok(TerrainCandidate {
                vehicle_id,
                position,
            })
        })
        .collect()
    }

    pub fn record_terrain_failure(
        &self,
        candidate: &TerrainCandidate,
        error_code: &str,
        retry_after_ms: i64,
        attempted_at_ms: i64,
    ) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        upsert_terrain_provenance(
            &transaction,
            candidate,
            None,
            None,
            None,
            None,
            "failed",
            Some(error_code),
            retry_after_ms,
            attempted_at_ms,
        )?;
        advance_terrain_cursor(&transaction, candidate, attempted_at_ms)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)
    }

    pub fn apply_terrain_result(
        &self,
        candidate: &TerrainCandidate,
        elevation_m: Option<i16>,
        tile_name: &str,
        tile_hash: &str,
        dataset_source: &str,
        dataset_version: &str,
        attempted_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let vehicle_key = candidate.vehicle_id.to_string();
        let current_json: String = transaction
            .query_row(
                "SELECT position_json FROM materialised_positions
                 WHERE vehicle_id = ?1 AND position_id = ?2",
                params![vehicle_key, candidate.position.id],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let mut position: ProjectionPosition =
            serde_json::from_str(&current_json).map_err(StoreError::DeserializeLifecycleRow)?;
        let changed = position.elevation.is_none() && elevation_m.is_some();
        if changed {
            position.elevation = elevation_m.map(i64::from);
            let position_json =
                serde_json::to_string(&position).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_positions SET position_json = ?3
                     WHERE vehicle_id = ?1 AND position_id = ?2",
                    params![vehicle_key, position.id, position_json],
                )
                .map_err(StoreError::LifecycleWrite)?;
            if let Some(drive_id) = position.drive_id {
                recompute_terrain_drive(&transaction, &vehicle_key, drive_id)?;
                let drive_json: String = transaction
                    .query_row(
                        "SELECT drive_json FROM materialised_drives
                         WHERE vehicle_id = ?1 AND drive_id = ?2",
                        params![vehicle_key, drive_id],
                        |row| row.get(0),
                    )
                    .map_err(StoreError::LifecycleWrite)?;
                record_sync_mutation_in_transaction(
                    &transaction,
                    candidate.vehicle_id,
                    "drive",
                    drive_id,
                    position.car_id,
                    "upsert",
                    &drive_json,
                )?;
            }
            record_sync_mutation_in_transaction(
                &transaction,
                candidate.vehicle_id,
                "position",
                position.id,
                position.car_id,
                "upsert",
                &position_json,
            )?;
            mark_export_dirty_in_transaction(&transaction, candidate.vehicle_id)?;
        }
        upsert_terrain_provenance(
            &transaction,
            candidate,
            Some(tile_name),
            Some(tile_hash),
            Some(dataset_source),
            Some(dataset_version),
            if elevation_m.is_some() {
                "success"
            } else {
                "void"
            },
            None,
            0,
            attempted_at_ms,
        )?;
        advance_terrain_cursor(&transaction, candidate, attempted_at_ms)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(changed)
    }

    /// The digest recorded when the Hub downloaded `tile_name` (G2-3f), if any.
    /// Files without one are unproven and are never admitted.
    pub fn terrain_tile_digest(
        &self,
        tile_name: &str,
    ) -> Result<Option<TerrainTileDigest>, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT sha256, recorded_at_ms FROM terrain_tile_digests WHERE tile_name = ?1",
                params![tile_name],
                |row| {
                    Ok(TerrainTileDigest {
                        sha256: row.get(0)?,
                        recorded_at_ms: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::Query)
    }

    /// Record the digest of a tile the Hub has just downloaded, replacing any
    /// earlier one for the same tile. Only the download path calls this.
    pub fn record_terrain_tile_digest(
        &self,
        tile_name: &str,
        sha256: &str,
        recorded_at_ms: i64,
    ) -> Result<(), StoreError> {
        if crate::terrain::TileId::parse(tile_name).is_err()
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || recorded_at_ms < 0
        {
            return Err(StoreError::InvalidTerrainTileDigest);
        }
        let connection = self.open()?;
        connection
            .execute(
                "INSERT INTO terrain_tile_digests(tile_name, sha256, recorded_at_ms)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(tile_name) DO UPDATE
                 SET sha256 = excluded.sha256, recorded_at_ms = excluded.recorded_at_ms",
                params![tile_name, sha256, recorded_at_ms],
            )
            .map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }

    pub fn publish_terrain_revision(
        &self,
        vehicle_id: Uuid,
        cursor_key: &CursorKey,
        minimum_free_bytes: u64,
    ) -> Result<bool, StoreError> {
        self.publish_terrain_revision_guarded(vehicle_id, cursor_key, minimum_free_bytes, &mut |_| {
            false
        })
    }

    /// [`Self::publish_terrain_revision`] for a caller that must not write after it lost
    /// the right to: `refused` is asked immediately before each independent mutation
    /// (the sequence reservation, the pack output and the snapshot's finalisation),
    /// with the name of the boundary, and a `true` ends the publication with
    /// [`StoreError::WriteRefused`] before that mutation.
    pub fn publish_terrain_revision_guarded(
        &self,
        vehicle_id: Uuid,
        cursor_key: &CursorKey,
        minimum_free_bytes: u64,
        refused: &mut dyn FnMut(&'static str) -> bool,
    ) -> Result<bool, StoreError> {
        if self.vehicle_has_v2_base(vehicle_id)? {
            // The durable terrain mutations are already in the live sync
            // journal. Once an immutable base exists, the normal export
            // outbox must publish those mutations as sparse deltas; creating
            // another full snapshot here would replace neither the base nor
            // its head and could incorrectly acknowledge the journal.
            return Ok(false);
        }
        let history = self.materialised_history(vehicle_id)?;
        let Some(car) = history.car.clone() else {
            return Err(StoreError::TerrainCarMissing(vehicle_id));
        };
        let connection = self.open()?;
        let (source_id, generation): (String, i64) = connection
            .query_row(
                "SELECT v.source_id, s.generation
                 FROM vehicles v JOIN sources s ON s.source_id = v.source_id
                 WHERE v.vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(StoreError::Query)?;
        let account_id = Uuid::parse_str(&source_id).map_err(|_| StoreError::InvalidSourceId)?;
        let generation =
            u64::try_from(generation).map_err(|_| StoreError::InvalidStoredSequence)?;
        let snapshot = ProjectionSnapshot {
            cars: vec![car],
            drives: history.drives,
            positions: history.positions,
            charges: history.charges,
            charge_samples: history.charge_samples,
        };
        let fingerprint = Sha256Digest::from_bytes(
            Sha256::digest(
                serde_json::to_vec(&(&snapshot, &history.states, &history.updates))
                    .map_err(StoreError::SerializeLifecycleRow)?,
            )
            .into(),
        );
        if self.snapshot_fingerprint_is_current(vehicle_id, fingerprint)? {
            return Ok(false);
        }
        // The collector invokes terrain publication under the same outer
        // publication gate as its outbox and lifecycle writes.
        if refused("terrain-publish-sequence") {
            return Err(StoreError::WriteRefused("terrain-publish-sequence"));
        }
        let sequence = self.next_full_snapshot_sequence_while_gated(vehicle_id)?;
        let binding = ProjectionBinding {
            installation_id: self.installation_id()?,
            account_id,
            vehicle_id,
            generation,
            selected_car_id: snapshot.cars[0].id,
        };
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: binding.clone(),
            sequence: SequenceRange {
                from_exclusive: sequence,
                to_inclusive: sequence,
            },
            snapshot: &snapshot,
        };
        let writer =
            ProjectionPackWriter::new(self.packs_dir()).with_minimum_free_bytes(minimum_free_bytes);
        if refused("terrain-publish-pack") {
            return Err(StoreError::WriteRefused("terrain-publish-pack"));
        }
        let built = writer
            .write_full_snapshot_with_states_and_updates(
                &request,
                &history.states,
                &history.updates,
            )
            .map_err(StoreError::TerrainPack)?;
        let manifest = request
            .signed_manifest_with_states_and_updates(
                &built,
                &history.states,
                &history.updates,
                cursor_key,
            )
            .map_err(StoreError::TerrainPack)?;
        if refused("terrain-publish-finalize") {
            return Err(StoreError::WriteRefused("terrain-publish-finalize"));
        }
        self.finalize_import_snapshot_with_binding(&manifest, fingerprint, &[], &binding)?;
        Ok(true)
    }

    /// Check database integrity, report quarantined lifecycle state, and remove
    /// orphaned transport packs that are not referenced in the manifest catalog.
    ///
    /// A quarantine is evidence of a semantic projection failure. Clearing it
    /// without reconstructing from the immutable journal would make a damaged
    /// cursor appear healthy, so this safe repair deliberately preserves it.
    ///
    /// The pass also drops the sealed successor of every rotation grace window
    /// that has ended, so a rotated device that never authenticates again does
    /// not keep one in the catalogue.
    pub fn repair(&self) -> Result<RepairReport, StoreError> {
        self.repair_at(retired_lineage_clock_ms()?)
    }

    fn repair_at(&self, now_ms: i64) -> Result<RepairReport, StoreError> {
        if now_ms < 0 {
            return Err(StoreError::LineageCatalogConflict);
        }
        let _publication_gate = self.try_acquire_publication_gate()?;
        let mut connection = self.open()?;
        let retired_cleanup_cutoff = now_ms.saturating_sub(RETIRED_LINEAGE_PACK_DELETE_GRACE_MS);
        self.verify_referenced_packs_at(now_ms)?;
        let quarantined_sessions_preserved: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM vehicle_lifecycle_state WHERE quarantined != 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Query)?;
        let quarantined_sessions_preserved = usize::try_from(quarantined_sessions_preserved)
            .map_err(|_| StoreError::InvalidStoredCount)?;

        let mut catalog_shas = std::collections::HashSet::new();
        for (sha, _, _) in referenced_pack_rows_at(&connection, retired_cleanup_cutoff)? {
            catalog_shas.insert(sha);
        }

        let sqlite_integrity = catalogue_quick_check_label(&connection)?;
        let (mut orphaned_packs_removed, mut freed_bytes) =
            cleanup_stale_pack_staging(self.packs_dir()).map_err(StoreError::PackStartupRepair)?;
        for packs_dir in [
            self.packs_dir().to_path_buf(),
            self.packs_dir().join("sha256"),
        ] {
            if let Ok(entries) = std::fs::read_dir(packs_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let is_orphaned = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|name| name.strip_suffix(".sqlite.zst"))
                        .is_some_and(|sha| !catalog_shas.contains(sha));
                    if is_orphaned {
                        if let Ok(metadata) = entry.metadata() {
                            freed_bytes += metadata.len();
                        }
                        if std::fs::remove_file(&path).is_ok() {
                            orphaned_packs_removed += 1;
                        }
                    }
                }
            }
        }
        // Retention metadata stays retryable if any validation or filesystem
        // cleanup above fails; make its deletion the final fallible operation.
        let transaction = connection.transaction().map_err(StoreError::Query)?;
        transaction
            .execute(
                "DELETE FROM sync_retired_lineages WHERE expires_at_ms <= ?1",
                params![retired_cleanup_cutoff],
            )
            .map_err(StoreError::LineageCatalog)?;
        transaction
            .execute(
                "DELETE FROM prepared_map_months AS p
                 WHERE NOT EXISTS (
                     SELECT 1 FROM pending_physical_v3_admissions AS h
                     WHERE h.vehicle_id = p.vehicle_id
                       AND h.receipt_id = p.input_receipt_id
                       AND h.serve_state = 'public_first'
                 ) AND NOT EXISTS (
                     SELECT 1 FROM retained_physical_v3_admissions AS r
                     WHERE r.vehicle_id = p.vehicle_id
                       AND r.receipt_id = p.input_receipt_id
                       AND r.expires_at_ms > ?1
                 )",
                params![retired_cleanup_cutoff],
            )
            .map_err(StoreError::LineageCatalog)?;
        transaction
            .execute(
                "DELETE FROM retained_physical_v3_admissions WHERE expires_at_ms <= ?1",
                params![retired_cleanup_cutoff],
            )
            .map_err(StoreError::LineageCatalog)?;
        // An ended grace window (`valid_until_ms <= now`, the bound a grace
        // retry and a grace authentication refuse at) has no use for the
        // successor sealed in it. The row and its digest stay.
        transaction
            .execute(
                "UPDATE paired_device_token_grace SET sealed_successor = NULL
                 WHERE valid_until_ms <= ?1 AND sealed_successor IS NOT NULL",
                params![now_ms],
            )
            .map_err(StoreError::RotateDevice)?;
        transaction.commit().map_err(StoreError::LineageCatalog)?;

        Ok(RepairReport {
            status: "ok".to_owned(),
            sqlite_integrity,
            quarantined_sessions_preserved,
            orphaned_packs_removed,
            freed_bytes,
        })
    }
}

/// The close sequence of C2 R46, run inside the lifecycle commit's
/// transaction for every charge in `commit.delta.charges`, in delta order.
///
/// 1. The row's samples: its stored open samples plus the delta's own, by id,
///    folded in constant space into the facts the cost rules read (the first
///    sample, the last two in sample order, the largest counter), never held
///    or sorted as a list.
/// 2. Label and tariff copy from the start-coordinates entry of the delta, by
///    the one geofence rule over the vehicle's fences (C1 R72): the label is
///    the fence name; the copy is the fence's tariff when complete and valid
///    ([`ChargeTariff::of_fence`]), with the session fee waived when the row
///    continues the previous row's plug-in (R44a). No entry, no match, or an
///    unusable tariff leaves the copy empty, and with no match the label too.
/// 3. `free_supercharging` of the car's settings, read once per commit; no
///    row, or a row of another car, means false, because a missing setting is
///    not an entitlement.
/// 4. The cost. A stored row with the same `(vehicle_id, charge_id)` and a
///    known cost keeps that cost and its tariff copy (R47: no field tells a
///    manual or imported cost from a computed one). Otherwise the cost is
///    [`calculate_charge_cost`] over the row's own copy and the context. The
///    stored row is matched by its key; its `car_id` column does not matter,
///    because a cost already stored must not be lost.
/// 5. The upsert and one `charge` sync mutation, skipped when the stored JSON
///    already equals the new JSON (R6), so a replay adds no mutation.
///
/// Energies are taken as `finalize_charge` produced them (R26). The car's
/// efficiency is recomputed by the caller once the closed charges' samples are
/// materialised too (R67), because the figure reads those samples. Nothing
/// here logs; the only identifiers that could be reported are ids (R7).
fn close_charges_in_transaction(
    transaction: &Transaction<'_>,
    commit: &LifecycleCommit<'_>,
) -> Result<(), StoreError> {
    if commit.delta.charges.is_empty() {
        return Ok(());
    }
    let vehicle_key = commit.vehicle_id.to_string();
    let fences = load_geofence_fences(transaction, commit.vehicle_id)?;
    let free_supercharging = match load_car_settings_row(transaction, commit.vehicle_id)? {
        Some((car_id, settings)) if car_id == commit.car_id => settings.free_supercharging,
        _ => false,
    };
    for charge in &commit.delta.charges {
        let mut row = charge.clone();
        // The cost rules read four facts of the samples, so they are folded in
        // constant space: the delta's own samples first, then the stored open
        // samples one row at a time, skipping the ones the delta carries again
        // (by id, in a set the size of the delta). The close of a long charge
        // therefore holds no sample list and sorts nothing (C2 R46 step 1).
        let mut facts = crate::lifecycle::ChargeSampleFacts::default();
        let mut carried_by_delta = std::collections::HashSet::new();
        for sample in commit
            .delta
            .charge_samples
            .iter()
            .filter(|sample| sample.charge_process_id == row.id)
        {
            if carried_by_delta.insert(sample.id) {
                facts.observe(sample);
            }
        }
        fold_open_charge_samples_for_parent(
            transaction,
            &vehicle_key,
            row.id,
            &carried_by_delta,
            &mut facts,
        )?;

        let fence = commit
            .delta
            .charge_start_coordinates
            .iter()
            .find(|(id, _, _)| *id == row.id)
            .and_then(|(_, latitude, longitude)| {
                crate::lifecycle::match_geofence(*latitude, *longitude, &fences)
            });
        row.geofence = fence.map(|fence| fence.name.clone());
        let continues = crate::lifecycle::charge_continues_plug_in_from(&row, &facts);
        let tariff = fence
            .and_then(crate::lifecycle::ChargeTariff::of_fence)
            .map(|tariff| if continues { tariff.waive_fee() } else { tariff });
        row.billing_type = tariff.map(|tariff| tariff.billing_type);
        row.cost_per_unit = tariff.and_then(|tariff| tariff.cost_per_unit);
        row.session_fee = tariff.and_then(|tariff| tariff.session_fee);

        let stored_json: Option<String> = transaction
            .query_row(
                "SELECT charge_json FROM materialised_charges
                 WHERE vehicle_id = ?1 AND charge_id = ?2",
                params![vehicle_key, row.id],
                |stored| stored.get(0),
            )
            .optional()
            .map_err(StoreError::LifecycleWrite)?;
        let stored_row = stored_json
            .as_deref()
            .map(serde_json::from_str::<crate::hub_pack::ProjectionCharge>)
            .transpose()
            .map_err(StoreError::DeserializeLifecycleRow)?;
        match stored_row.filter(|stored| stored.cost.is_some()) {
            Some(stored) => {
                row.cost = stored.cost;
                row.billing_type = stored.billing_type;
                row.cost_per_unit = stored.cost_per_unit;
                row.session_fee = stored.session_fee;
            }
            None => {
                let context = crate::lifecycle::ChargeCostContext {
                    free_supercharging,
                    span_observed: crate::lifecycle::charge_span_observed_from(&row, &facts),
                };
                row.cost = crate::lifecycle::calculate_charge_cost(
                    &row,
                    crate::lifecycle::ChargeTariff::of_charge(&row),
                    context,
                );
            }
        }

        let charge_json =
            serde_json::to_string(&row).map_err(StoreError::SerializeLifecycleRow)?;
        if stored_json.as_deref() == Some(charge_json.as_str()) {
            continue;
        }
        transaction
            .execute(
                "INSERT INTO materialised_charges(
                    vehicle_id, charge_id, car_id, charge_json
                ) VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(vehicle_id, charge_id) DO UPDATE SET
                    car_id = excluded.car_id,
                    charge_json = excluded.charge_json",
                params![vehicle_key, row.id, commit.car_id, charge_json],
            )
            .map_err(StoreError::LifecycleWrite)?;
        record_sync_mutation_in_transaction(
            transaction,
            commit.vehicle_id,
            "charge",
            row.id,
            commit.car_id,
            "upsert",
            &charge_json,
        )?;
    }
    Ok(())
}

/// The walk of the retroactive fill (C2 R50): fill every unknown cost of a Hub
/// charge whose start the one geofence rule puts in `target`, with `tariff`
/// (the target's complete and valid tariff). Returns the rows changed.
///
/// The walk takes the vehicle's charge rows by id, a bounded page at a time,
/// and checks the cheap conditions before it loads a row's samples: only a row
/// that passes them can need the continuation and span checks (R43a, R44a),
/// which read the stored samples.
fn fill_missing_charge_costs_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    target: &crate::lifecycle::GeofenceFence,
    tariff: crate::lifecycle::ChargeTariff,
) -> Result<u64, StoreError> {
    let fences = load_geofence_fences(transaction, vehicle_id)?;
    let bound = charge_import_bound(transaction, vehicle_id)?;
    let settings = load_car_settings_row(transaction, vehicle_id)?;
    let vehicle_key = vehicle_id.to_string();
    let mut changed = 0_u64;
    let mut after_id = 0_i64;
    loop {
        let page = materialised_charge_page(transaction, vehicle_id, after_id)?;
        let Some(last) = page.last().map(|(id, _)| *id) else {
            break;
        };
        after_id = last;
        for (charge_id, charge_json) in page {
            let mut row: crate::hub_pack::ProjectionCharge =
                serde_json::from_str(&charge_json).map_err(StoreError::DeserializeLifecycleRow)?;
            if row.cost.is_some() || bound.is_imported(charge_id) {
                continue;
            }
            let (Some(latitude), Some(longitude)) = (row.start_latitude, row.start_longitude)
            else {
                continue;
            };
            let in_target = crate::lifecycle::match_geofence(latitude, longitude, &fences)
                .is_some_and(|fence| fence.source_geofence_id == target.source_geofence_id);
            if !in_target {
                continue;
            }
            let facts = fold_materialised_charge_samples(transaction, vehicle_id, charge_id)?;
            let tariff = if crate::lifecycle::charge_continues_plug_in_from(&row, &facts) {
                tariff.waive_fee()
            } else {
                tariff
            };
            // The setting belongs to one car (R46 step 3): a row of another
            // car is not entitled by it.
            let free_supercharging = settings.as_ref().is_some_and(|(car_id, settings)| {
                *car_id == row.car_id && settings.free_supercharging
            });
            let context = crate::lifecycle::ChargeCostContext {
                free_supercharging,
                span_observed: crate::lifecycle::charge_span_observed_from(&row, &facts),
            };
            let Some(cost) = crate::lifecycle::calculate_charge_cost(&row, Some(tariff), context)
            else {
                continue;
            };
            row.cost = Some(cost);
            row.billing_type = Some(tariff.billing_type);
            row.cost_per_unit = tariff.cost_per_unit;
            row.session_fee = tariff.session_fee;
            let filled_json =
                serde_json::to_string(&row).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_charges SET charge_json = ?3
                     WHERE vehicle_id = ?1 AND charge_id = ?2",
                    params![vehicle_key, charge_id, filled_json],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                vehicle_id,
                "charge",
                charge_id,
                row.car_id,
                "upsert",
                &filled_json,
            )?;
            changed += 1;
        }
    }
    Ok(changed)
}
