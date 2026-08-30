// SPDX-License-Identifier: AGPL-3.0-only

fn publish_manifest_in_transaction(
    transaction: &Transaction<'_>,
    manifest: &SyncManifest,
    binding: Option<&ProjectionBinding>,
) -> Result<(), StoreError> {
    validate_manifest_for_catalogue(manifest)?;
    match binding {
        Some(binding) => validate_immutable_v2_base_binding(manifest, binding)?,
        None if manifest.schema == HUB_PROJECTION_SCHEMA_V2 => {
            return Err(StoreError::ImmutableBaseBindingMissing(manifest.vehicle_id));
        }
        None => {}
    }
    // `SyncManifest` describes a full snapshot or generic V1 incremental
    // transfer. It has no typed-delta marker, so it can never safely extend
    // an immutable V2 projection base. Import successors must go through
    // `finalize_import_delta_successor`, which receives a `LineageDelta`
    // written by the typed projection-delta writer.
    if manifest.schema == HUB_PROJECTION_SCHEMA_V2
        && transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_bases WHERE vehicle_id = ?1)",
                params![manifest.vehicle_id.to_string()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(StoreError::LineageCatalog)?
    {
        return Err(StoreError::ImportDeltaRequiresBaseBinding);
    }
    let payload = serde_json::to_vec(manifest).map_err(StoreError::SerializeManifest)?;
    let snapshot_id = manifest.snapshot_id.to_string();
    let vehicle_id = manifest.vehicle_id.to_string();
    let head_sequence =
        i64::try_from(manifest.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let current = transaction.query_row(
        "SELECT snapshot_id, head_sequence FROM sync_manifests WHERE vehicle_id = ?1 ORDER BY head_sequence DESC LIMIT 1",
        params![vehicle_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    ).optional().map_err(StoreError::Query)?;
    if let Some((current_snapshot_id, current_sequence)) = current {
        let current_sequence =
            u64::try_from(current_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
        if current_sequence > manifest.head_sequence
            || (current_sequence == manifest.head_sequence && current_snapshot_id != snapshot_id)
        {
            return Err(StoreError::StaleManifest {
                vehicle_id: manifest.vehicle_id,
                attempted: manifest.head_sequence,
                current: current_sequence,
            });
        }
    }
    transaction
        .execute(
            "INSERT INTO sync_manifests(snapshot_id, vehicle_id, head_sequence, manifest_json)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(snapshot_id) DO UPDATE SET vehicle_id = excluded.vehicle_id,
            head_sequence = excluded.head_sequence, manifest_json = excluded.manifest_json",
            params![snapshot_id, vehicle_id, head_sequence, payload],
        )
        .map_err(StoreError::PublishManifest)?;
    transaction
        .execute(
            "DELETE FROM sync_packs WHERE snapshot_id = ?1",
            params![manifest.snapshot_id.to_string()],
        )
        .map_err(StoreError::PublishManifest)?;
    for pack in &manifest.chunks {
        transaction.execute(
            "INSERT INTO sync_packs(sha256, snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![pack.sha256.to_string(), manifest.snapshot_id.to_string(), i64::from(pack.ordinal),
                pack.relative_path, i64::try_from(pack.compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
                i64::try_from(pack.uncompressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?],
        ).map_err(StoreError::PublishManifest)?;
    }
    if manifest.schema == HUB_PROJECTION_SCHEMA_V2 && !manifest.chunks.is_empty() {
        let pack_digest = manifest.chunks[0].sha256.to_string();
        let packs_json =
            serde_json::to_vec(&manifest.chunks).map_err(StoreError::SerializeManifest)?;
        let terminal_cursor = serde_json::to_string(&manifest.terminal_cursor)
            .map_err(StoreError::SerializeManifest)?;
        transaction
            .execute(
                "INSERT INTO sync_bases(vehicle_id, snapshot_id, base_sequence, base_digest, packs_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    manifest.vehicle_id.to_string(),
                    manifest.snapshot_id.to_string(),
                    head_sequence,
                    pack_digest,
                    packs_json
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        // This is deliberately part of the same transaction as the base and
        // head catalogue writes. A V2 base must never become visible before
        // its exact immutable source/car binding is durable.
        record_immutable_v2_base_binding_in_transaction(
            transaction,
            manifest,
            binding.ok_or(StoreError::ImmutableBaseBindingMissing(manifest.vehicle_id))?,
        )?;
        transaction
            .execute(
                "INSERT INTO sync_heads(vehicle_id, base_snapshot_id, head_sequence, head_digest, terminal_cursor)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    manifest.vehicle_id.to_string(),
                    manifest.snapshot_id.to_string(),
                    head_sequence,
                    pack_digest,
                    terminal_cursor
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        transaction
            .execute(
                "UPDATE sync_mutations SET published = 1, claimed_until_ms = 0 WHERE vehicle_id = ?1 AND published = 0",
                params![manifest.vehicle_id.to_string()],
            )
            .map_err(StoreError::LineageCatalog)?;
    }
    Ok(())
}

fn record_snapshot_fingerprint_in_transaction(
    transaction: &Transaction<'_>,
    manifest: &SyncManifest,
    fingerprint: Sha256Digest,
) -> Result<(), StoreError> {
    validate_manifest_for_catalogue(manifest)?;
    let head_sequence =
        i64::try_from(manifest.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let associated: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_manifests
                 WHERE snapshot_id = ?1 AND vehicle_id = ?2 AND head_sequence = ?3
            )",
            params![
                manifest.snapshot_id.to_string(),
                manifest.vehicle_id.to_string(),
                head_sequence
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::PublishManifest)?;
    if !associated {
        return Err(StoreError::FingerprintManifestMissing(manifest.snapshot_id));
    }
    transaction
        .execute(
            "INSERT INTO snapshot_fingerprints(
                vehicle_id, fingerprint_sha256, snapshot_id, head_sequence
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                fingerprint_sha256 = excluded.fingerprint_sha256,
                snapshot_id = excluded.snapshot_id,
                head_sequence = excluded.head_sequence",
            params![
                manifest.vehicle_id.to_string(),
                fingerprint.as_bytes().as_slice(),
                manifest.snapshot_id.to_string(),
                head_sequence,
            ],
        )
        .map_err(StoreError::PublishManifest)?;
    Ok(())
}

fn upsert_geofences_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    geofences: &[crate::teslamate_projection::TeslaMateGeofence],
) -> Result<usize, StoreError> {
    let mut inserted = 0;
    for geofence in geofences {
        let Some((latitude, longitude, radius_m)) = geofence.valid_geometry() else {
            continue;
        };
        if geofence.name.trim().is_empty() || geofence.name.len() > 256 {
            continue;
        }
        inserted += transaction.execute(
            "INSERT INTO geofences(vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                billing_type, cost_per_unit, session_fee) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(vehicle_id, source_geofence_id) DO NOTHING",
            params![vehicle_id.to_string(), geofence.id, geofence.name.trim(), latitude, longitude, radius_m,
                geofence.billing_type.map(crate::hub_pack::GeofenceBillingType::as_str), geofence.cost_per_unit, geofence.session_fee],
        ).map_err(StoreError::LifecycleWrite)?;
        transaction
            .execute(
                "UPDATE geofences SET name=?3, latitude=?4, longitude=?5, radius_m=?6,
                billing_type=COALESCE(?7,billing_type), cost_per_unit=COALESCE(?8,cost_per_unit),
                session_fee=COALESCE(?9,session_fee) WHERE vehicle_id=?1 AND source_geofence_id=?2",
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
                    geofence.session_fee
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
    }
    Ok(inserted)
}

fn promote_imported_open_session_in_transaction(
    transaction: &Transaction<'_>,
    source_id: Uuid,
    vehicle_id: Uuid,
    car_id: i64,
    session: &TeslaMateOpenSession,
    updated_at_ms: i64,
    expected: Option<(i64, i64)>,
) -> Result<OpenSessionSeedReport, StoreError> {
    if source_id.is_nil() || vehicle_id.is_nil() || car_id <= 0 {
        return Err(StoreError::InvalidLifecycleCarId);
    }
    validate_timestamp("open session updated_at_ms", updated_at_ms)?;
    session
        .validate()
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
    let previous = load_lifecycle_state_in_transaction(transaction, vehicle_id)?;
    if let Some((last_observation_id, prior_updated_at_ms)) = expected {
        let actual = previous
            .as_ref()
            .map(|state| (state.last_observation_id, state.updated_at_ms));
        if actual != Some((last_observation_id, prior_updated_at_ms))
            && (actual.is_some() || (last_observation_id, prior_updated_at_ms) != (0, 0))
        {
            return Err(StoreError::ImportGenerationConflict);
        }
    }
    let previous_state = previous
        .as_ref()
        .map(|state| {
            crate::lifecycle::OpenSessionState::decode(&state.open_session_json)
                .map_err(|_| StoreError::InvalidLifecycleSession)
        })
        .transpose()?;
    let seeded = crate::lifecycle::seed_imported_open_session_state(
        source_id,
        session,
        previous_state.as_ref(),
    )
    .map_err(|_| StoreError::InvalidLifecycleSession)?;
    ensure_source_exists(transaction, source_id)?;
    ensure_vehicle_source(transaction, vehicle_id, source_id)?;
    let source = source_id.to_string();
    let vehicle = vehicle_id.to_string();
    transaction
        .execute(
            "DELETE FROM lifecycle_open_rows WHERE source_id=?1 AND vehicle_id=?2",
            params![source, vehicle],
        )
        .map_err(StoreError::LifecycleWrite)?;
    transaction
        .execute(
            "DELETE FROM lifecycle_source_watermarks WHERE source_id=?1 AND vehicle_id=?2",
            params![source, vehicle],
        )
        .map_err(StoreError::LifecycleWrite)?;
    let mut inserted = 0;
    if let Some(row) = &session.drive {
        inserted += insert_open_row(
            transaction,
            &source,
            "drives",
            row.id,
            &vehicle,
            car_id,
            "drive",
            None,
            row,
        )?;
    }
    for row in &session.drive_positions {
        inserted += insert_open_row(
            transaction,
            &source,
            "positions",
            row.id,
            &vehicle,
            car_id,
            "position",
            row.drive_id,
            row,
        )?;
    }
    if let Some(row) = &session.charge {
        inserted += insert_open_row(
            transaction,
            &source,
            "charging_processes",
            row.id,
            &vehicle,
            car_id,
            "charge",
            None,
            row,
        )?;
    }
    for row in &session.charge_samples {
        inserted += insert_open_row(
            transaction,
            &source,
            "charges",
            row.id,
            &vehicle,
            car_id,
            "charge_sample",
            Some(row.charging_process_id),
            row,
        )?;
    }
    if let Some(row) = &session.state {
        inserted += insert_open_row(
            transaction,
            &source,
            "states",
            row.id,
            &vehicle,
            car_id,
            "state",
            None,
            row,
        )?;
    }
    for row in &session.standalone_positions {
        inserted += insert_open_row(
            transaction,
            &source,
            "positions",
            row.id,
            &vehicle,
            car_id,
            "standalone_position",
            None,
            row,
        )?;
    }
    let mut standalone_positions_inserted = 0;
    for row in &session.standalone_positions {
        let position = crate::lifecycle::imported_position(row);
        let json = serde_json::to_string(&position).map_err(StoreError::SerializeLifecycleRow)?;
        standalone_positions_inserted += transaction.execute(
            "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json,
                speed, power, est_battery_range_km, fan_status, driver_temp_setting, passenger_temp_setting,
                is_climate_on, is_rear_defroster_on, is_front_defroster_on, battery_heater, battery_heater_on,
                battery_heater_no_power, tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
             ON CONFLICT(vehicle_id, position_id) DO NOTHING",
            params![vehicle, position.id, car_id, json, position.speed, position.power, position.est_battery_range_km,
                position.fan_status, position.driver_temp_setting, position.passenger_temp_setting,
                position.is_climate_on.map(i64::from), position.is_rear_defroster_on.map(i64::from),
                position.is_front_defroster_on.map(i64::from), position.battery_heater.map(i64::from),
                position.battery_heater_on.map(i64::from), position.battery_heater_no_power.map(i64::from),
                position.tpms_pressure_fl, position.tpms_pressure_fr, position.tpms_pressure_rl, position.tpms_pressure_rr],
        ).map_err(StoreError::LifecycleWrite)?;
    }
    let watermarks = [
        ("drives", session.watermarks.drives),
        ("positions", session.watermarks.positions),
        ("charging_processes", session.watermarks.charging_processes),
        ("charges", session.watermarks.charges),
        ("states", session.watermarks.states),
        ("updates", session.watermarks.updates),
    ];
    for (domain, watermark) in watermarks {
        transaction.execute(
            "INSERT INTO lifecycle_source_watermarks(source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(source_id, vehicle_id, domain) DO UPDATE SET
             max_source_row_id=MAX(max_source_row_id, excluded.max_source_row_id),
             max_timestamp_ms=MAX(max_timestamp_ms, excluded.max_timestamp_ms)",
            params![source, vehicle, domain, watermark.max_id, watermark.max_timestamp_ms],
        ).map_err(StoreError::LifecycleWrite)?;
    }
    let json = seeded
        .encode()
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
    transaction.execute(
        "INSERT INTO vehicle_lifecycle_state(vehicle_id, car_id, last_observation_id, open_session_json, quarantined, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, 0, ?5) ON CONFLICT(vehicle_id) DO UPDATE SET car_id=excluded.car_id,
         open_session_json=excluded.open_session_json, updated_at_ms=MAX(updated_at_ms, excluded.updated_at_ms)",
        params![vehicle, car_id, previous.as_ref().map_or(0, |state| state.last_observation_id), json, updated_at_ms],
    ).map_err(StoreError::LifecycleWrite)?;
    mark_export_dirty_in_transaction(transaction, vehicle_id)?;
    Ok(OpenSessionSeedReport {
        provisional_rows_inserted: inserted,
        standalone_positions_inserted,
        watermarks_written: watermarks.len(),
        no_op: false,
    })
}
