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
        let fence = geofence_fence_by_id(&transaction, vehicle_id, source_geofence_id)?
            .ok_or(StoreError::UnknownGeofence(source_geofence_id))?;
        let tariff = fence
            .billing_type
            .map(|billing_type| crate::lifecycle::ChargeTariff {
                billing_type,
                cost_per_unit: fence.cost_per_unit,
                session_fee: fence.session_fee,
            });
        let mut updated = 0_u64;
        let mut after_id = 0_i64;
        loop {
            let page = materialised_charge_page(&transaction, vehicle_id, after_id)?;
            let Some(last) = page.last().map(|(id, _)| *id) else {
                break;
            };
            after_id = last;
            for (charge_id, charge_json) in page {
                let mut charge: crate::hub_pack::ProjectionCharge =
                    serde_json::from_str(&charge_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?;
                if charge.cost.is_some()
                    || !matches!(
                        (charge.start_latitude, charge.start_longitude),
                        (Some(latitude), Some(longitude))
                            if crate::lifecycle::match_geofence(
                                latitude,
                                longitude,
                                std::slice::from_ref(&fence),
                            )
                            .is_some()
                    )
                {
                    continue;
                }
                let Some(cost) = crate::lifecycle::calculate_charge_cost(
                    charge.fast_charger_type.as_deref(),
                    false,
                    charge.charge_energy_added,
                    charge.charge_energy_used_kwh,
                    charge.duration_min,
                    tariff,
                ) else {
                    continue;
                };
                charge.cost = Some(cost);
                let payload =
                    serde_json::to_string(&charge).map_err(StoreError::SerializeLifecycleRow)?;
                transaction
                    .execute(
                        "UPDATE materialised_charges SET charge_json = ?3
                         WHERE vehicle_id = ?1 AND charge_id = ?2",
                        params![vehicle_id.to_string(), charge_id, payload],
                    )
                    .map_err(StoreError::LifecycleWrite)?;
                record_sync_mutation_in_transaction(
                    &transaction,
                    vehicle_id,
                    "charge",
                    charge_id,
                    charge.car_id,
                    "upsert",
                    &payload,
                )?;
                updated += 1;
            }
        }
        if updated > 0 {
            mark_export_dirty_in_transaction(&transaction, vehicle_id)?;
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(updated)
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

    fn commit_lifecycle_delta_in_transaction(
        transaction: &Transaction<'_>,
        commit: &LifecycleCommit<'_>,
    ) -> Result<(), StoreError> {
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
        // When a drive/charge closes without a full in-memory child buffer
        // (incremental path), pull durable open children once for materialization.
        for drive in &delta.drives {
            let open_positions =
                load_open_positions_for_parent(transaction, &vehicle_key, drive.id)?;
            for position in open_positions {
                if !delta
                    .positions
                    .iter()
                    .any(|existing| existing.id == position.id)
                {
                    delta.positions.push(position);
                }
            }
        }
        for charge in &delta.charges {
            let open_samples =
                load_open_charge_samples_for_parent(transaction, &vehicle_key, charge.id)?;
            for sample in open_samples {
                if !delta
                    .charge_samples
                    .iter()
                    .any(|existing| existing.id == sample.id)
                {
                    delta.charge_samples.push(sample);
                }
            }
        }
        let fences = load_geofence_fences(transaction, commit.vehicle_id)?;
        crate::lifecycle::apply_geofence_labels(&mut delta, &fences);
        let free_supercharging = transaction
            .query_row(
                "SELECT free_supercharging FROM car_settings
                 WHERE vehicle_id = ?1 AND car_id = ?2",
                params![commit.vehicle_id.to_string(), commit.car_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(StoreError::LifecycleWrite)?
            .unwrap_or(0)
            != 0;

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
        for position in &delta.positions {
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
        }
        for charge in &delta.charges {
            let mut charge = charge.clone();
            let charge_samples = delta
                .charge_samples
                .iter()
                .filter(|sample| sample.charge_process_id == charge.id)
                .cloned()
                .collect::<Vec<_>>();
            if let Some(energy_used_kwh) =
                crate::lifecycle::calculate_energy_used_kwh(&charge_samples)
            {
                charge.charge_energy_used_kwh = Some(energy_used_kwh);
            }
            let start_fence = delta
                .charge_start_coordinates
                .iter()
                .find(|(id, _, _)| *id == charge.id)
                .and_then(|(_, latitude, longitude)| {
                    crate::lifecycle::match_geofence(*latitude, *longitude, &fences)
                });
            if charge.geofence.is_none() {
                charge.geofence = start_fence.map(|fence| fence.name.clone());
            }
            if charge.billing_type.is_none() {
                charge.billing_type = start_fence.and_then(|fence| fence.billing_type);
            }
            if charge.cost_per_unit.is_none() {
                charge.cost_per_unit = start_fence.and_then(|fence| fence.cost_per_unit);
            }
            if charge.session_fee.is_none() {
                charge.session_fee = start_fence.and_then(|fence| fence.session_fee);
            }
            if charge.cost.is_none() {
                charge.cost = crate::lifecycle::calculate_charge_cost(
                    charge.fast_charger_type.as_deref(),
                    free_supercharging,
                    charge.charge_energy_added,
                    charge.charge_energy_used_kwh,
                    charge.duration_min,
                    start_fence.and_then(|fence| {
                        fence
                            .billing_type
                            .map(|billing_type| crate::lifecycle::ChargeTariff {
                                billing_type,
                                cost_per_unit: fence.cost_per_unit,
                                session_fee: fence.session_fee,
                            })
                    }),
                );
            }
            let charge_json =
                serde_json::to_string(&charge).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_charges(
                        vehicle_id, charge_id, car_id, charge_json
                    ) VALUES (?1, ?2, ?3, ?4)
                    ON CONFLICT(vehicle_id, charge_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        charge_json = excluded.charge_json",
                    params![
                        commit.vehicle_id.to_string(),
                        charge.id,
                        commit.car_id,
                        charge_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "charge",
                charge.id,
                commit.car_id,
                "upsert",
                &charge_json,
            )?;
        }
        if !delta.charges.is_empty() {
            recompute_car_efficiency(transaction, commit.vehicle_id, commit.car_id)?;
        }
        for sample in &delta.charge_samples {
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

    pub fn publish_terrain_revision(
        &self,
        vehicle_id: Uuid,
        cursor_key: &CursorKey,
        minimum_free_bytes: u64,
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
        self.finalize_import_snapshot_with_binding(&manifest, fingerprint, &[], &binding)?;
        Ok(true)
    }

    /// Check database integrity, report quarantined lifecycle state, and remove
    /// orphaned transport packs that are not referenced in the manifest catalog.
    ///
    /// A quarantine is evidence of a semantic projection failure. Clearing it
    /// without reconstructing from the immutable journal would make a damaged
    /// cursor appear healthy, so this safe repair deliberately preserves it.
    pub fn repair(&self) -> Result<RepairReport, StoreError> {
        self.repair_at(retired_lineage_clock_ms()?)
    }

    fn repair_at(&self, now_ms: i64) -> Result<RepairReport, StoreError> {
        if now_ms < 0 {
            return Err(StoreError::LineageCatalogConflict);
        }
        let _publication_gate = self.try_acquire_publication_gate()?;
        let connection = self.open()?;
        let retired_cleanup_cutoff = now_ms.saturating_sub(RETIRED_LINEAGE_PACK_DELETE_GRACE_MS);
        connection
            .execute(
                "DELETE FROM sync_retired_lineages WHERE expires_at_ms <= ?1",
                params![retired_cleanup_cutoff],
            )
            .map_err(StoreError::LineageCatalog)?;
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

        Ok(RepairReport {
            status: "ok".to_owned(),
            sqlite_integrity,
            quarantined_sessions_preserved,
            orphaned_packs_removed,
            freed_bytes,
        })
    }
}
