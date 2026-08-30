// SPDX-License-Identifier: AGPL-3.0-only

fn load_car_settings_row(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<Option<(i64, ProjectionCarSettings)>, StoreError> {
    transaction
        .query_row(
            "SELECT car_id, enabled, use_streaming_api, suspend_after_idle_min,
                    suspend_min, req_not_unlocked, free_supercharging,
                    lfp_battery, suspend_min_resolved
             FROM car_settings WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    ProjectionCarSettings {
                        enabled: row.get::<_, i64>(1)? != 0,
                        use_streaming_api: row.get::<_, i64>(2)? != 0,
                        suspend_after_idle_min: row.get(3)?,
                        suspend_min: row.get(4)?,
                        req_not_unlocked: row.get::<_, i64>(5)? != 0,
                        free_supercharging: row.get::<_, i64>(6)? != 0,
                        lfp_battery: row.get::<_, i64>(7)? != 0,
                        suspend_min_resolved: row.get::<_, i64>(8)? != 0,
                    },
                ))
            },
        )
        .optional()
        .map_err(StoreError::Query)
}

fn record_sync_mutation_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    entity: &str,
    entity_id: i64,
    car_id: i64,
    operation: &str,
    payload_json: &str,
) -> Result<(), StoreError> {
    let next_revision: i64 = transaction
        .query_row(
            "INSERT INTO sync_mutation_sequences(vehicle_id, next_revision)
             VALUES (?1, 2)
             ON CONFLICT(vehicle_id) DO UPDATE SET next_revision = next_revision + 1
             RETURNING next_revision - 1",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "INSERT INTO sync_mutations(
                vehicle_id, revision, entity, entity_id, car_id,
                operation, payload_json, published, claimed_until_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0)",
            params![
                vehicle_id.to_string(),
                next_revision,
                entity,
                entity_id,
                car_id,
                operation,
                payload_json,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    Ok(())
}

fn parse_sync_entity(value: &str) -> Option<ProjectionDeltaEntity> {
    match value {
        "car" => Some(ProjectionDeltaEntity::Car),
        "car_setting" => Some(ProjectionDeltaEntity::CarSetting),
        "geofence" => Some(ProjectionDeltaEntity::Geofence),
        "address" => Some(ProjectionDeltaEntity::Address),
        "drive" => Some(ProjectionDeltaEntity::Drive),
        "position" => Some(ProjectionDeltaEntity::Position),
        "charge" => Some(ProjectionDeltaEntity::Charge),
        "charge_sample" => Some(ProjectionDeltaEntity::ChargeSample),
        "state" => Some(ProjectionDeltaEntity::State),
        "update" => Some(ProjectionDeltaEntity::Update),
        _ => None,
    }
}

fn load_projection_json<T: DeserializeOwned>(
    connection: &Connection,
    table: &str,
    column: &str,
    id_column: &str,
    mutation: &SyncMutation,
) -> Result<T, StoreError> {
    let sql = format!("SELECT {column} FROM {table} WHERE vehicle_id = ?1 AND {id_column} = ?2");
    let json: Option<String> = connection
        .query_row(
            &sql,
            params![mutation.vehicle_id.to_string(), mutation.entity_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    let json = json.ok_or_else(|| {
        StoreError::SyncMutation(format!(
            "missing materialised {} {}",
            mutation.entity, mutation.entity_id
        ))
    })?;
    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)
}

fn insert_live_delta_span_in_transaction(
    transaction: &Transaction<'_>,
    claim: &SyncMutationClaim,
    delta: &LineageDelta,
) -> Result<(), StoreError> {
    let vehicle_key = claim.vehicle_id.to_string();
    transaction
        .execute(
            "INSERT INTO sync_live_delta_spans(
                vehicle_id, from_sequence, to_sequence,
                from_revision, to_revision, pack_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(vehicle_id, from_sequence, to_sequence) DO NOTHING",
            params![
                vehicle_key.as_str(),
                i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                claim.from_revision,
                claim.to_revision,
                delta.pack_digest.to_string(),
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    let stored: (i64, i64, String) = transaction
        .query_row(
            "SELECT from_revision, to_revision, pack_digest
             FROM sync_live_delta_spans
             WHERE vehicle_id = ?1 AND from_sequence = ?2 AND to_sequence = ?3",
            params![
                vehicle_key,
                i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(StoreError::LineageCatalog)?;
    if stored
        != (
            claim.from_revision,
            claim.to_revision,
            delta.pack_digest.to_string(),
        )
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

fn address_lookup_key(point: crate::location::Wgs84Point) -> String {
    format!("{:.6}:{:.6}", point.latitude, point.longitude)
}

fn advance_terrain_cursor(
    transaction: &Transaction<'_>,
    candidate: &TerrainCandidate,
    attempted_at_ms: i64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO terrain_enrichment_state(
                vehicle_id, cursor_position_id, updated_at_ms
             ) VALUES (?1, ?2, ?3)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                cursor_position_id = MAX(cursor_position_id, excluded.cursor_position_id),
                updated_at_ms = excluded.updated_at_ms",
            params![
                candidate.vehicle_id.to_string(),
                candidate.position.id,
                attempted_at_ms,
            ],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn upsert_terrain_provenance(
    transaction: &Transaction<'_>,
    candidate: &TerrainCandidate,
    tile_name: Option<&str>,
    tile_hash: Option<&str>,
    dataset_source: Option<&str>,
    dataset_version: Option<&str>,
    status: &str,
    error_code: Option<&str>,
    retry_after_ms: i64,
    attempted_at_ms: i64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO terrain_elevation_provenance(
                vehicle_id, position_id, drive_id, latitude, longitude,
                elevation_m, tile_name, tile_hash, dataset_source, dataset_version,
                status, error_code, attempts, attempted_at_ms, retry_after_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                       COALESCE((SELECT attempts FROM terrain_elevation_provenance
                                 WHERE vehicle_id = ?1 AND position_id = ?2), 0) + 1,
                       ?13, ?14)
             ON CONFLICT(vehicle_id, position_id) DO UPDATE SET
                drive_id = excluded.drive_id,
                latitude = excluded.latitude,
                longitude = excluded.longitude,
                elevation_m = excluded.elevation_m,
                tile_name = excluded.tile_name,
                tile_hash = excluded.tile_hash,
                dataset_source = excluded.dataset_source,
                dataset_version = excluded.dataset_version,
                status = excluded.status,
                error_code = excluded.error_code,
                attempts = terrain_elevation_provenance.attempts + 1,
                attempted_at_ms = excluded.attempted_at_ms,
                retry_after_ms = excluded.retry_after_ms",
            params![
                candidate.vehicle_id.to_string(),
                candidate.position.id,
                candidate.position.drive_id,
                candidate.position.latitude,
                candidate.position.longitude,
                candidate.position.elevation,
                tile_name,
                tile_hash,
                dataset_source,
                dataset_version,
                status,
                error_code,
                attempted_at_ms,
                retry_after_ms,
            ],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn recompute_car_efficiency(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    car_id: i64,
) -> Result<(), StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT charge_json FROM materialised_charges
             WHERE vehicle_id = ?1 AND car_id = ?2 ORDER BY charge_id",
        )
        .map_err(StoreError::LifecycleWrite)?;
    let charges = statement
        .query_map(params![vehicle_id.to_string(), car_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(StoreError::LifecycleWrite)?;
    let specifications = [(5_u32, 8_usize), (4, 5), (3, 3), (2, 2)];
    let mut groups = [
        HashMap::<i64, usize>::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    ];
    for value in charges {
        let value = value.map_err(StoreError::LifecycleWrite)?;
        let charge: crate::hub_pack::ProjectionCharge =
            serde_json::from_str(&value).map_err(StoreError::DeserializeLifecycleRow)?;
        let (Some(duration_min), Some(end_battery_level), Some(start_range), Some(end_range)) = (
            charge.duration_min,
            charge.end_battery_level,
            charge.start_rated_range_km,
            charge.end_rated_range_km,
        ) else {
            continue;
        };
        let Some(energy_added) = charge.charge_energy_added else {
            continue;
        };
        let range_added = end_range - start_range;
        if duration_min <= 10
            || end_battery_level > 95
            || energy_added <= 0.0
            || !energy_added.is_finite()
            || !range_added.is_finite()
            || range_added == 0.0
        {
            continue;
        }
        let factor = energy_added / range_added;
        if !factor.is_finite() || factor <= 0.0 {
            continue;
        }
        for ((precision, _), counts) in specifications.iter().zip(&mut groups) {
            let scale = 10_i64.pow(*precision) as f64;
            let key = (factor * scale).round() as i64;
            *counts.entry(key).or_default() += 1;
        }
    }
    let mut selected = None;
    for ((precision, threshold), groups) in specifications.into_iter().zip(groups) {
        let scale = 10_i64.pow(precision) as f64;
        selected = groups
            .into_iter()
            .filter(|(_, count)| *count >= threshold)
            .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
            .map(|(factor, _)| factor as f64 / scale);
        if selected.is_some() {
            break;
        }
    }
    let Some(efficiency) = selected else {
        return Ok(());
    };
    let current: Option<String> = transaction
        .query_row(
            "SELECT car_json FROM materialised_cars
             WHERE vehicle_id = ?1 AND car_id = ?2",
            params![vehicle_id.to_string(), car_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::LifecycleWrite)?;
    let Some(current) = current else {
        return Ok(());
    };
    let mut car: ProjectionCar =
        serde_json::from_str(&current).map_err(StoreError::DeserializeLifecycleRow)?;
    if car.efficiency_wh_per_km == Some(efficiency) {
        return Ok(());
    }
    car.efficiency_wh_per_km = Some(efficiency);
    let payload = serde_json::to_string(&car).map_err(StoreError::SerializeLifecycleRow)?;
    transaction
        .execute(
            "UPDATE materialised_cars SET car_json = ?3
             WHERE vehicle_id = ?1 AND car_id = ?2",
            params![vehicle_id.to_string(), car_id, payload],
        )
        .map_err(StoreError::LifecycleWrite)?;
    record_sync_mutation_in_transaction(
        transaction,
        vehicle_id,
        "car",
        car_id,
        car_id,
        "upsert",
        &payload,
    )?;
    Ok(())
}

fn recompute_terrain_drive(
    transaction: &Transaction<'_>,
    vehicle_id: &str,
    drive_id: i64,
) -> Result<(), StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT position_json FROM materialised_positions
             WHERE vehicle_id = ?1 AND drive_id = ?2
             ORDER BY position_id ASC",
        )
        .map_err(StoreError::LifecycleWrite)?;
    let rows = statement
        .query_map(params![vehicle_id, drive_id], |row| row.get::<_, String>(0))
        .map_err(StoreError::LifecycleWrite)?;
    let positions: Vec<ProjectionPosition> = rows
        .map(|row| {
            let json = row.map_err(StoreError::LifecycleWrite)?;
            serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)
        })
        .collect::<Result<_, _>>()?;
    let (ascent, descent) = terrain_elevation_totals(&positions);
    let drive_json: String = transaction
        .query_row(
            "SELECT drive_json FROM materialised_drives
             WHERE vehicle_id = ?1 AND drive_id = ?2",
            params![vehicle_id, drive_id],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    let mut drive: ProjectionDrive =
        serde_json::from_str(&drive_json).map_err(StoreError::DeserializeLifecycleRow)?;
    drive.ascent = Some(ascent);
    drive.descent = Some(descent);
    let drive_json = serde_json::to_string(&drive).map_err(StoreError::SerializeLifecycleRow)?;
    transaction
        .execute(
            "UPDATE materialised_drives SET drive_json = ?3, ascent = ?4, descent = ?5
             WHERE vehicle_id = ?1 AND drive_id = ?2",
            params![vehicle_id, drive_id, drive_json, ascent, descent],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn terrain_elevation_totals(positions: &[ProjectionPosition]) -> (i64, i64) {
    let mut previous = None;
    let mut ascent = 0_i64;
    let mut descent = 0_i64;
    for elevation in positions.iter().filter_map(|position| position.elevation) {
        if let Some(previous_elevation) = previous {
            let delta = elevation - previous_elevation;
            if delta > 0 {
                ascent = ascent.saturating_add(delta);
            } else if delta < 0 {
                descent = descent.saturating_add(delta.unsigned_abs() as i64);
            }
        }
        previous = Some(elevation);
    }
    (
        if ascent >= 32_768 { 0 } else { ascent },
        if descent >= 32_768 { 0 } else { descent },
    )
}

fn validate_address_cache_record(record: &AddressCacheRecord) -> Result<(), StoreError> {
    if record.osm_type.is_empty()
        || record.osm_type.len() > 32
        || record.osm_type.chars().any(char::is_control)
        || record.osm_id <= 0
        || record.display_name.trim().is_empty()
        || record.display_name.len() > MAX_DISPLAY_NAME_BYTES
        || invalid_address_text(record.name.as_deref())
        || invalid_address_text(record.house_number.as_deref())
        || invalid_address_text(record.road.as_deref())
        || invalid_address_text(record.neighbourhood.as_deref())
        || invalid_address_text(record.city.as_deref())
        || invalid_address_text(record.county.as_deref())
        || invalid_address_text(record.postcode.as_deref())
        || invalid_address_text(record.state.as_deref())
        || invalid_address_text(record.state_district.as_deref())
        || invalid_address_text(record.country.as_deref())
        || record
            .latitude
            .is_some_and(|latitude| !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude))
        || record.longitude.is_some_and(|longitude| {
            !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude)
        })
        || record.raw_json.as_deref().is_some_and(|raw| {
            raw.len() > MAX_ADDRESS_RAW_JSON_BYTES || serde_json::from_str::<Value>(raw).is_err()
        })
        || !record.lookup_latitude.is_finite()
        || !(-90.0..=90.0).contains(&record.lookup_latitude)
        || !record.lookup_longitude.is_finite()
        || !(-180.0..=180.0).contains(&record.lookup_longitude)
        || record.looked_up_at_ms < 0
    {
        return Err(StoreError::InvalidAddressCache);
    }
    Ok(())
}

fn invalid_address_text(value: Option<&str>) -> bool {
    value.is_some_and(|text| {
        text.len() > MAX_DISPLAY_NAME_BYTES || text.chars().any(char::is_control)
    })
}

fn load_geofence_fences(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<Vec<crate::lifecycle::GeofenceFence>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT name, latitude, longitude, radius_m, billing_type,
                    cost_per_unit, session_fee
             FROM geofences WHERE vehicle_id = ?1 ORDER BY source_geofence_id",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_id.to_string()], |row| {
            Ok(crate::lifecycle::GeofenceFence {
                name: row.get(0)?,
                latitude: row.get(1)?,
                longitude: row.get(2)?,
                radius_m: row.get(3)?,
                billing_type: row
                    .get::<_, Option<String>>(4)?
                    .map(|value| match value.as_str() {
                        "per_kwh" => crate::hub_pack::GeofenceBillingType::PerKwh,
                        "per_minute" => crate::hub_pack::GeofenceBillingType::PerMinute,
                        _ => crate::hub_pack::GeofenceBillingType::PerKwh,
                    }),
                cost_per_unit: row.get(5)?,
                session_fee: row.get(6)?,
            })
        })
        .map_err(StoreError::Query)?;
    rows.map(|row| row.map_err(StoreError::Query)).collect()
}

fn geofence_fence_by_id(
    connection: &Connection,
    vehicle_id: Uuid,
    source_geofence_id: i64,
) -> Result<Option<crate::lifecycle::GeofenceFence>, StoreError> {
    connection
        .query_row(
            "SELECT name, latitude, longitude, radius_m, billing_type,
                    cost_per_unit, session_fee
             FROM geofences WHERE vehicle_id = ?1 AND source_geofence_id = ?2",
            params![vehicle_id.to_string(), source_geofence_id],
            |row| {
                Ok(crate::lifecycle::GeofenceFence {
                    name: row.get(0)?,
                    latitude: row.get(1)?,
                    longitude: row.get(2)?,
                    radius_m: row.get(3)?,
                    billing_type: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|value| value.parse().ok()),
                    cost_per_unit: row.get(5)?,
                    session_fee: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::Query)
}

fn materialised_charge_page(
    connection: &Connection,
    vehicle_id: Uuid,
    after_id: i64,
) -> Result<Vec<(i64, String)>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT charge_id, charge_json FROM materialised_charges
             WHERE vehicle_id = ?1 AND charge_id > ?2
             ORDER BY charge_id LIMIT 256",
        )
        .map_err(StoreError::Query)?;
    statement
        .query_map(params![vehicle_id.to_string(), after_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)
}

fn materialised_drive_page(
    connection: &Connection,
    vehicle_id: Uuid,
    after_id: i64,
) -> Result<Vec<(i64, String)>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT drive_id, drive_json FROM materialised_drives
             WHERE vehicle_id = ?1 AND drive_id > ?2
             ORDER BY drive_id LIMIT 256",
        )
        .map_err(StoreError::Query)?;
    statement
        .query_map(params![vehicle_id.to_string(), after_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)
}

fn relabel_materialised_locations_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<(), StoreError> {
    let fences = load_geofence_fences(transaction, vehicle_id)?;
    let mut changed = false;
    let mut after_id = 0_i64;
    loop {
        let page = materialised_drive_page(transaction, vehicle_id, after_id)?;
        let Some(last) = page.last().map(|(id, _)| *id) else {
            break;
        };
        after_id = last;
        for (drive_id, drive_json) in page {
            let mut drive: crate::hub_pack::ProjectionDrive =
                serde_json::from_str(&drive_json).map_err(StoreError::DeserializeLifecycleRow)?;
            let start = match (drive.start_latitude, drive.start_longitude) {
                (Some(latitude), Some(longitude)) => {
                    crate::lifecycle::match_geofence_name(latitude, longitude, &fences)
                }
                _ => drive.start_geofence.clone(),
            };
            let end = match (drive.end_latitude, drive.end_longitude) {
                (Some(latitude), Some(longitude)) => {
                    crate::lifecycle::match_geofence_name(latitude, longitude, &fences)
                }
                _ => drive.end_geofence.clone(),
            };
            if drive.start_geofence == start && drive.end_geofence == end {
                continue;
            }
            drive.start_geofence = start;
            drive.end_geofence = end;
            let payload =
                serde_json::to_string(&drive).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_drives SET drive_json = ?3
                     WHERE vehicle_id = ?1 AND drive_id = ?2",
                    params![vehicle_id.to_string(), drive_id, payload],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                vehicle_id,
                "drive",
                drive_id,
                drive.car_id,
                "upsert",
                &payload,
            )?;
            changed = true;
        }
    }

    after_id = 0;
    loop {
        let page = materialised_charge_page(transaction, vehicle_id, after_id)?;
        let Some(last) = page.last().map(|(id, _)| *id) else {
            break;
        };
        after_id = last;
        for (charge_id, charge_json) in page {
            let mut charge: crate::hub_pack::ProjectionCharge =
                serde_json::from_str(&charge_json).map_err(StoreError::DeserializeLifecycleRow)?;
            let geofence = match (charge.start_latitude, charge.start_longitude) {
                (Some(latitude), Some(longitude)) => {
                    crate::lifecycle::match_geofence_name(latitude, longitude, &fences)
                }
                _ => charge.geofence.clone(),
            };
            if charge.geofence == geofence {
                continue;
            }
            charge.geofence = geofence;
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
                transaction,
                vehicle_id,
                "charge",
                charge_id,
                charge.car_id,
                "upsert",
                &payload,
            )?;
            changed = true;
        }
    }
    if changed {
        mark_export_dirty_in_transaction(transaction, vehicle_id)?;
    }
    Ok(())
}

fn enqueue_address_jobs(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
    delta: &crate::lifecycle::LifecycleDelta,
) -> Result<(), StoreError> {
    for drive in &delta.drives {
        let endpoints = [
            (
                "start_address",
                drive.start_latitude,
                drive.start_longitude,
                drive.start_address.is_some(),
            ),
            (
                "end_address",
                drive.end_latitude,
                drive.end_longitude,
                drive.end_address.is_some(),
            ),
        ];
        for (field, latitude, longitude, already_labeled) in endpoints {
            if already_labeled {
                continue;
            }
            let (Some(latitude), Some(longitude)) = (latitude, longitude) else {
                continue;
            };
            if latitude.is_finite()
                && longitude.is_finite()
                && (-90.0..=90.0).contains(&latitude)
                && (-180.0..=180.0).contains(&longitude)
            {
                insert_address_job(
                    transaction,
                    vehicle_id,
                    "drive",
                    drive.id,
                    field,
                    latitude,
                    longitude,
                )?;
            }
        }
    }
    for charge in &delta.charges {
        if charge.address.is_some() {
            continue;
        }
        let Some((_, latitude, longitude)) = delta
            .charge_start_coordinates
            .iter()
            .find(|(id, _, _)| *id == charge.id)
        else {
            continue;
        };
        if latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(latitude)
            && (-180.0..=180.0).contains(longitude)
        {
            insert_address_job(
                transaction,
                vehicle_id,
                "charge",
                charge.id,
                "address",
                *latitude,
                *longitude,
            )?;
        }
    }
    Ok(())
}

fn insert_address_job(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
    target_type: &str,
    target_id: i64,
    field: &str,
    latitude: f64,
    longitude: f64,
) -> Result<(), StoreError> {
    let job_key = format!("{vehicle_id}:{target_type}:{target_id}:{field}");
    transaction
        .execute(
            "INSERT INTO address_enrichment_jobs(
                job_key, vehicle_id, target_type, target_id, field,
                latitude, longitude, status, attempts, next_attempt_ms,
                lease_until_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', 0, 0, 0)
             ON CONFLICT(vehicle_id, target_type, target_id, field) DO NOTHING",
            params![
                job_key,
                vehicle_id.to_string(),
                target_type,
                target_id,
                field,
                latitude,
                longitude
            ],
        )
        .map_err(StoreError::AddressEnrichmentWrite)?;
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_file_hex(path: &Path) -> Result<String, StoreError> {
    let mut file = fs::File::open(path).map_err(StoreError::OpenBackupPack)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(StoreError::ReadBackupPack)?;
        if read == 0 {
            return Ok(hex::encode(digest.finalize()));
        }
        digest.update(&buffer[..read]);
    }
}

fn immutable_catalogue_fingerprint(
    database_path: &Path,
) -> Result<ImmutableCatalogueFingerprint, StoreError> {
    let metadata = fs::symlink_metadata(database_path).map_err(StoreError::InspectCatalogue)?;
    if !metadata.file_type().is_file() {
        return Err(StoreError::InvalidCataloguePath);
    }

    let mut wal_name = database_path.as_os_str().to_os_string();
    wal_name.push("-wal");
    let wal_path = PathBuf::from(wal_name);
    match fs::symlink_metadata(&wal_path) {
        Ok(wal) if wal.len() != 0 => return Err(StoreError::PendingCatalogueWal),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(StoreError::InspectCatalogue(error)),
    }

    let mut file = File::open(database_path).map_err(StoreError::ReadCatalogue)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(StoreError::ReadCatalogue)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(ImmutableCatalogueFingerprint {
        bytes: metadata.len(),
        sha256: hex::encode(digest.finalize()),
    })
}

fn persistent_journal_mode(database_path: &Path) -> Result<String, StoreError> {
    let mut file = File::open(database_path).map_err(StoreError::ReadCatalogue)?;
    let mut header = [0_u8; 20];
    file.read_exact(&mut header)
        .map_err(StoreError::ReadCatalogue)?;
    if &header[..16] != b"SQLite format 3\0" {
        return Ok("invalid".to_owned());
    }
    Ok(match (header[18], header[19]) {
        (2, 2) => "wal",
        (1, 1) => "rollback",
        _ => "invalid",
    }
    .to_owned())
}
