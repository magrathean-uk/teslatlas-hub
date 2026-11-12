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
        // A collector may have recorded rows while the source snapshot was
        // being built. The imported base cannot prove which of those rows it
        // contains, so leave their journal entries pending for a typed delta.
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

fn promote_imported_open_session_in_transaction(
    transaction: &Transaction<'_>,
    source_id: Uuid,
    vehicle_id: Uuid,
    car_id: i64,
    session: &SourceDbOpenSession,
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
    // No watermark row of any domain is deleted (C2 R80 (b)): the writer below
    // only raises each pair, because a replacement or promotion must never
    // lower how far an imported class reaches (for `charging_processes`, C2
    // R5's import bound W(V)).
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
    // Positions go by instant (design M03-SEAM-R3, section 4): inside the
    // Hub's coverage the Hub's own rows stand, so a source position there is
    // not written; outside it the source's is. Source ids never reach the
    // Hub's range, so no source position can take a Hub row's key.
    let coverage = import_seam_coverage_in_transaction(transaction, &vehicle)?;
    let mut standalone_positions_inserted = 0;
    for row in &session.standalone_positions {
        if crate::db::hub_ids::is_hub_id(row.id) || row.id > crate::db::hub_ids::HUB_ID_MAX {
            return Err(StoreError::InvalidImportGeneration);
        }
        if coverage.contains(row.date_ms) {
            continue;
        }
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
    // C2 R80 (g), C2 R80 (d), C1 R84 (b), C1 R90, C9 R58: raise-only merge,
    // all-source cursor raise, then the boundary hook for changed Positions.
    let watermarks = snapshot_watermark_pairs(&session.watermarks);
    let changed_domains =
        raise_source_watermarks_in_transaction(transaction, source_id, vehicle_id, &watermarks)?;
    let mut seeded = seeded;
    raise_id_cursors_above_watermarks(transaction, vehicle_id, &mut seeded)?;
    if changed_domains.contains(&WatermarkDomain::Positions) {
        terrain_after_import_boundary_raise_in_transaction(transaction, vehicle_id, updated_at_ms)?;
    }
    seed_imported_version_from_updates(transaction, vehicle_id, &mut seeded)?;
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

/// The collection coverage of one vehicle, read inside a publication
/// transaction. A window still open in the store has no cutoff yet, so the
/// publication refuses rather than guess one.
fn import_seam_coverage_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_key: &str,
) -> Result<crate::import::source_db::seam::HubCoverage, StoreError> {
    crate::import::source_db::seam::HubCoverage::for_store(transaction, vehicle_key)
        .map_err(|_| StoreError::ImportGenerationConflict)
}

/// One stored drive as the seam weighs it.
struct StoredSeamDrive {
    car_id: i64,
    start_ms: i64,
    end_ms: Option<i64>,
}

/// One Hub-range position as the seam weighs it.
struct StoredSeamPosition {
    car_id: i64,
    at_ms: i64,
    drive_id: Option<i64>,
}

/// The import seam applied to the drives and positions in the store, inside
/// the publication transaction (design M03-SEAM-R3, section 4).
///
/// Ownership is the id range: a Hub-range row is the Hub's. A stored drive
/// with a source id is the Hub's inherited session (one id on both sides)
/// when the prior import did not list it or a Hub-range position belongs to
/// it; every other stored source-id drive is an earlier import's. The pure
/// [`plan_seam`](crate::import::source_db::seam::plan_seam) decides from the
/// coverage in the store, those rows, the source drives and the recorded
/// links, and this applies it: winning source drives are written, Hub drives
/// that lost are removed with a tombstone, source drives that lost are not
/// written (and removed with a tombstone if an earlier import stored them),
/// stale source rows go with a tombstone, Hub positions outside coverage go
/// with a tombstone, Hub positions whose drive lost move to the winner, kept
/// sessions widen to hold their children, and every loser is recorded in
/// `import_seam_links`. Applied again to the same source, the plan finds its
/// links and superseded rows and changes nothing.
fn apply_import_seam_to_drives_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    drives: &[ProjectionDrive],
    prior_imported_drive_ids: &HashSet<i64>,
) -> Result<(), StoreError> {
    use crate::db::hub_ids::{HUB_ID_BASE, HUB_ID_MAX, is_hub_id};
    use crate::import::source_db::seam::{
        ChildRow, HubRows, SeamLinks, SessionRow, SourceRows, plan_seam,
    };

    let vehicle_key = vehicle_id.to_string();
    let coverage = import_seam_coverage_in_transaction(transaction, &vehicle_key)?;

    let stored_drives: std::collections::BTreeMap<i64, StoredSeamDrive> = {
        let mut statement = transaction
            .prepare(
                "SELECT drive_id, car_id,
                        CAST(json_extract(drive_json, '$.start_date_ms') AS INTEGER),
                        CAST(json_extract(drive_json, '$.end_date_ms') AS INTEGER)
                   FROM materialised_drives WHERE vehicle_id = ?1 ORDER BY drive_id ASC",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![vehicle_key.as_str()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })
            .map_err(StoreError::Query)?;
        let mut stored = std::collections::BTreeMap::new();
        for row in rows {
            let (id, car_id, start_ms, end_ms) = row.map_err(StoreError::Query)?;
            // A stored drive without a start cannot be weighed; it stays as
            // it is and is never planned away.
            let Some(start_ms) = start_ms else { continue };
            stored.insert(
                id,
                StoredSeamDrive {
                    car_id,
                    start_ms,
                    end_ms,
                },
            );
        }
        stored
    };
    let hub_positions: std::collections::BTreeMap<i64, StoredSeamPosition> = {
        let mut statement = transaction
            .prepare(
                "SELECT position_id, car_id, drive_id,
                        CAST(json_extract(position_json, '$.date_ms') AS INTEGER)
                   FROM materialised_positions
                  WHERE vehicle_id = ?1 AND position_id BETWEEN ?2 AND ?3
                  ORDER BY position_id ASC",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(
                params![vehicle_key.as_str(), HUB_ID_BASE, HUB_ID_MAX],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .map_err(StoreError::Query)?;
        let mut positions = std::collections::BTreeMap::new();
        for row in rows {
            let (id, car_id, drive_id, at_ms) = row.map_err(StoreError::Query)?;
            // A Hub position without an instant cannot be placed; it stays.
            let Some(at_ms) = at_ms else { continue };
            positions.insert(
                id,
                StoredSeamPosition {
                    car_id,
                    at_ms,
                    drive_id,
                },
            );
        }
        positions
    };

    let held_by_hub_position: HashSet<i64> = hub_positions
        .values()
        .filter_map(|position| position.drive_id)
        .collect();
    let mut hub = HubRows::default();
    let mut hub_side: HashSet<i64> = HashSet::new();
    for (id, drive) in &stored_drives {
        let inherited = !prior_imported_drive_ids.contains(id) || held_by_hub_position.contains(id);
        if is_hub_id(*id) || inherited {
            hub.drives.push(SessionRow {
                id: *id,
                start_ms: drive.start_ms,
                end_ms: drive.end_ms,
            });
            hub_side.insert(*id);
        } else {
            hub.imported_drives.push(*id);
        }
    }
    hub.positions = hub_positions
        .iter()
        .map(|(id, position)| ChildRow {
            id: *id,
            at_ms: position.at_ms,
            parent: position.drive_id,
        })
        .collect();
    let source = SourceRows {
        drives: drives
            .iter()
            .map(|drive| SessionRow {
                id: drive.id,
                start_ms: drive.start_date_ms,
                end_ms: Some(drive.end_date_ms),
            })
            .collect(),
        ..SourceRows::default()
    };
    let links = SeamLinks {
        drives: seam_links_in_transaction(transaction, &vehicle_key, "drive")?,
        ..SeamLinks::default()
    };
    let plan = plan_seam(&coverage, &hub, &source, &links);
    let drive_plan = &plan.drives;
    let position_plan = &plan.positions;

    let widened: HashMap<i64, (i64, Option<i64>)> = drive_plan
        .widened
        .iter()
        .map(|bounds| (bounds.id, (bounds.start_ms, bounds.end_ms)))
        .collect();

    // Hub drives that lost: removed with a tombstone. Their positions are
    // decided by instant below, never because their drive lost.
    for link in &drive_plan.superseded {
        if !is_hub_id(link.loser_id) && !hub_side.contains(&link.loser_id) {
            return Err(StoreError::InvalidImportGeneration);
        }
        if let Some(drive) = stored_drives.get(&link.loser_id) {
            remove_seam_drive_in_transaction(transaction, vehicle_id, link.loser_id, drive.car_id)?;
        }
    }
    // Source drives that lost, and stale source rows: an earlier import's
    // copy goes with a tombstone; a Hub-range row is never removed here.
    let source_gone = drive_plan
        .skipped
        .iter()
        .map(|link| link.loser_id)
        .chain(drive_plan.stale.iter().copied());
    for id in source_gone {
        if is_hub_id(id) {
            return Err(StoreError::InvalidImportGeneration);
        }
        if let Some(drive) = stored_drives.get(&id) {
            remove_seam_drive_in_transaction(transaction, vehicle_id, id, drive.car_id)?;
        }
    }

    // Winning source drives, an inherited session's source copy among them.
    let source_by_id: HashMap<i64, &ProjectionDrive> =
        drives.iter().map(|drive| (drive.id, drive)).collect();
    for id in &drive_plan.imported {
        let Some(drive) = source_by_id.get(id) else {
            return Err(StoreError::InvalidImportGeneration);
        };
        let mut drive = (*drive).clone();
        if let Some((start_ms, end_ms)) = widened.get(id) {
            drive.start_date_ms = *start_ms;
            drive.end_date_ms = end_ms.unwrap_or(drive.end_date_ms);
        }
        let changed = upsert_imported_drive_in_transaction(transaction, &vehicle_key, &drive)?;
        if changed && hub_side.contains(id) {
            // The Hub's inherited row now holds the source copy.
            let drive_json =
                serde_json::to_string(&drive).map_err(StoreError::SerializeLifecycleRow)?;
            record_sync_mutation_in_transaction(
                transaction,
                vehicle_id,
                "drive",
                drive.id,
                drive.car_id,
                "upsert",
                &drive_json,
            )?;
        }
    }
    // Kept Hub sessions that widen to hold the children they gained.
    for id in &drive_plan.hub_kept {
        if let Some((start_ms, end_ms)) = widened.get(id) {
            widen_hub_drive_in_transaction(transaction, vehicle_id, *id, *start_ms, *end_ms)?;
        }
    }

    // Hub positions by instant: outside coverage they go with a tombstone.
    for id in &position_plan.hub_removed {
        let Some(position) = hub_positions.get(id) else {
            continue;
        };
        transaction
            .execute(
                "DELETE FROM materialised_positions WHERE vehicle_id = ?1 AND position_id = ?2",
                params![vehicle_key.as_str(), id],
            )
            .map_err(StoreError::LifecycleWrite)?;
        record_sync_mutation_in_transaction(
            transaction,
            vehicle_id,
            "position",
            *id,
            position.car_id,
            "tombstone",
            "{}",
        )?;
    }
    // Kept Hub positions whose drive lost move to the winner.
    for moved in &position_plan.moved_hub {
        move_hub_position_in_transaction(transaction, vehicle_id, moved.id, moved.to)?;
    }

    let decided: Vec<_> = drive_plan
        .skipped
        .iter()
        .chain(&drive_plan.superseded)
        .copied()
        .collect();
    record_seam_links_in_transaction(
        transaction,
        &vehicle_key,
        "drive",
        &decided,
        &drive_plan.links_dropped,
    )
}

/// Stores one imported drive; an unchanged row is not rewritten. Returns
/// whether the row was written.
fn upsert_imported_drive_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_key: &str,
    drive: &ProjectionDrive,
) -> Result<bool, StoreError> {
    let drive_json = serde_json::to_string(drive).map_err(StoreError::SerializeLifecycleRow)?;
    let written = transaction
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
                descent = excluded.descent
             WHERE materialised_drives.car_id IS NOT excluded.car_id
                OR materialised_drives.drive_json IS NOT excluded.drive_json
                OR materialised_drives.inside_temp_avg IS NOT excluded.inside_temp_avg
                OR materialised_drives.power_max IS NOT excluded.power_max
                OR materialised_drives.power_min IS NOT excluded.power_min
                OR materialised_drives.start_ideal_range_km IS NOT excluded.start_ideal_range_km
                OR materialised_drives.end_ideal_range_km IS NOT excluded.end_ideal_range_km
                OR materialised_drives.ascent IS NOT excluded.ascent
                OR materialised_drives.descent IS NOT excluded.descent",
            params![
                vehicle_key,
                drive.id,
                drive.car_id,
                drive_json,
                drive.inside_temp_avg,
                drive.power_max,
                drive.power_min,
                drive.start_ideal_range_km,
                drive.end_ideal_range_km,
                drive.ascent,
                drive.descent,
            ],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(written > 0)
}

/// Removes one drive the seam decided against, with its pending address work,
/// and journals its tombstone. Its positions are not touched: positions go by
/// instant, never because their drive lost.
fn remove_seam_drive_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    drive_id: i64,
    car_id: i64,
) -> Result<(), StoreError> {
    let vehicle_key = vehicle_id.to_string();
    transaction
        .execute(
            "DELETE FROM address_enrichment_jobs
              WHERE vehicle_id = ?1 AND target_type = 'drive' AND target_id = ?2",
            params![vehicle_key.as_str(), drive_id],
        )
        .map_err(StoreError::LifecycleWrite)?;
    let removed = transaction
        .execute(
            "DELETE FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = ?2",
            params![vehicle_key.as_str(), drive_id],
        )
        .map_err(StoreError::LifecycleWrite)?;
    if removed == 0 {
        return Ok(());
    }
    record_sync_mutation_in_transaction(
        transaction,
        vehicle_id,
        "drive",
        drive_id,
        car_id,
        "tombstone",
        "{}",
    )
}

/// Widens a kept Hub drive's bounds to hold the children it gained; its
/// totals stay its own. Journals the changed row.
fn widen_hub_drive_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    drive_id: i64,
    start_ms: i64,
    end_ms: Option<i64>,
) -> Result<(), StoreError> {
    let vehicle_key = vehicle_id.to_string();
    let Some((car_id, drive_json)) = transaction
        .query_row(
            "SELECT car_id, drive_json FROM materialised_drives
              WHERE vehicle_id = ?1 AND drive_id = ?2",
            params![vehicle_key.as_str(), drive_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(StoreError::Query)?
    else {
        return Ok(());
    };
    let mut value: Value =
        serde_json::from_str(&drive_json).map_err(|_| StoreError::InvalidLifecycleSession)?;
    let Some(object) = value.as_object_mut() else {
        return Err(StoreError::InvalidLifecycleSession);
    };
    object.insert("start_date_ms".into(), Value::from(start_ms));
    if let Some(end_ms) = end_ms {
        object.insert("end_date_ms".into(), Value::from(end_ms));
    }
    let widened = value.to_string();
    if widened == drive_json {
        return Ok(());
    }
    transaction
        .execute(
            "UPDATE materialised_drives SET drive_json = ?3 WHERE vehicle_id = ?1 AND drive_id = ?2",
            params![vehicle_key.as_str(), drive_id, widened],
        )
        .map_err(StoreError::LifecycleWrite)?;
    record_sync_mutation_in_transaction(
        transaction,
        vehicle_id,
        "drive",
        drive_id,
        car_id,
        "upsert",
        &widened,
    )
}

/// Moves one kept Hub position to the drive that won its instant and
/// journals the changed row.
fn move_hub_position_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    position_id: i64,
    drive_id: i64,
) -> Result<(), StoreError> {
    let vehicle_key = vehicle_id.to_string();
    let Some((car_id, position_json)) = transaction
        .query_row(
            "SELECT car_id, position_json FROM materialised_positions
              WHERE vehicle_id = ?1 AND position_id = ?2",
            params![vehicle_key.as_str(), position_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(StoreError::Query)?
    else {
        return Ok(());
    };
    let mut value: Value =
        serde_json::from_str(&position_json).map_err(|_| StoreError::InvalidLifecycleSession)?;
    let Some(object) = value.as_object_mut() else {
        return Err(StoreError::InvalidLifecycleSession);
    };
    object.insert("drive_id".into(), Value::from(drive_id));
    let moved = value.to_string();
    transaction
        .execute(
            "UPDATE materialised_positions SET drive_id = ?3, position_json = ?4
              WHERE vehicle_id = ?1 AND position_id = ?2",
            params![vehicle_key.as_str(), position_id, drive_id, moved],
        )
        .map_err(StoreError::LifecycleWrite)?;
    record_sync_mutation_in_transaction(
        transaction,
        vehicle_id,
        "position",
        position_id,
        car_id,
        "upsert",
        &moved,
    )
}
