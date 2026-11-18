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
    promote_imported_open_session_with_admitted_drives_in_transaction(
        transaction,
        source_id,
        vehicle_id,
        car_id,
        session,
        updated_at_ms,
        expected,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn promote_imported_open_session_with_admitted_drives_in_transaction(
    transaction: &Transaction<'_>,
    source_id: Uuid,
    vehicle_id: Uuid,
    car_id: i64,
    session: &SourceDbOpenSession,
    updated_at_ms: i64,
    expected: Option<(i64, i64)>,
    admitted_completed_drives: Option<WatermarkPair>,
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
    // M03-T18: once the vehicle has coverage, an import never discards the
    // lifecycle's open drive the Hub owns. The seam settles it first when the
    // source decides it, so one still open here is one the seam kept (or one
    // a window still open is collecting): it stays open with its open rows,
    // and the source's open drive is not taken over while it stands.
    let vehicle_key = vehicle_id.to_string();
    let kept_open = match previous_state.as_ref() {
        Some(state) if state.open_drive.is_some() => {
            match import_seam_open_drive_in_connection(transaction, &vehicle_key)? {
                Some(open) => {
                    let covered =
                        match crate::db::coverage_intervals_for_vehicle(transaction, &vehicle_key) {
                            Ok(intervals) => !intervals.is_empty(),
                            Err(_) => true,
                        };
                    covered.then_some(open.session.id)
                }
                None => None,
            }
        }
        _ => None,
    };
    // The same for the lifecycle's open charge the Hub owns (M03-T18, second
    // part): the seam settles it first when the source decides it, so one
    // still open here stays open with its open rows, and the source's open
    // charge is not taken over while it stands.
    let kept_open_charge = match previous_state.as_ref() {
        Some(state) if state.open_charge.is_some() => {
            match import_seam_owned_open_charge_in_connection(transaction, &vehicle_key)? {
                Some(open) => {
                    let covered =
                        match crate::db::coverage_intervals_for_vehicle(transaction, &vehicle_key) {
                            Ok(intervals) => !intervals.is_empty(),
                            Err(_) => true,
                        };
                    covered.then_some(open.session.id)
                }
                None => None,
            }
        }
        _ => None,
    };
    // The staged maximum describes the raw snapshot. A planned seam supplies
    // the reach of the completed drives it admitted. Count an open source
    // drive only when it will actually be inherited.
    let mut handover_watermarks = session.watermarks.clone();
    if let Some(mut admitted) = admitted_completed_drives {
        if let Some(drive) = session.drive.as_ref().filter(|_| kept_open.is_none()) {
            admitted = merge_watermark(
                admitted,
                WatermarkPair {
                    max_source_row_id: Some(drive.id),
                    max_timestamp_ms: Some(drive.start_date_ms),
                },
            );
        }
        handover_watermarks.drives = admitted.into();
    }
    let mut seeded = crate::lifecycle::seed_imported_open_session_state_with_watermarks(
        source_id,
        session,
        previous_state.as_ref(),
        &handover_watermarks,
    )
    .map_err(|_| StoreError::InvalidLifecycleSession)?;
    if kept_open.is_some()
        && let Some(previous) = previous_state.as_ref()
    {
        seeded.open_drive = previous.open_drive.clone();
    }
    if kept_open_charge.is_some()
        && let Some(previous) = previous_state.as_ref()
    {
        seeded.open_charge = previous.open_charge.clone();
    }
    // M03-T20: the phase follows the final open session, kept and imported
    // together, with the seeding's precedence (an open drive, then an open
    // charge); the previous phase is never restored.
    if kept_open.is_some() || kept_open_charge.is_some() {
        if seeded.open_drive.is_some() {
            seeded.phase = crate::lifecycle::VehiclePhase::Driving;
        } else if seeded.open_charge.is_some() {
            seeded.phase = crate::lifecycle::VehiclePhase::Charging;
        }
    }
    ensure_source_exists(transaction, source_id)?;
    ensure_vehicle_source(transaction, vehicle_id, source_id)?;
    let source = source_id.to_string();
    let vehicle = vehicle_id.to_string();
    let cleared = match (kept_open, kept_open_charge) {
        (None, None) => transaction.execute(
            "DELETE FROM lifecycle_open_rows WHERE source_id=?1 AND vehicle_id=?2",
            params![source, vehicle],
        ),
        (Some(drive_id), None) => transaction.execute(
            "DELETE FROM lifecycle_open_rows WHERE source_id=?1 AND vehicle_id=?2
                AND NOT ((domain = 'position' AND parent_source_row_id IS ?3)
                      OR (domain = 'drive' AND source_row_id = ?3))",
            params![source, vehicle, drive_id],
        ),
        // A kept open charge keeps its rows too; -1 is never a row id, so a
        // session that is not kept matches nothing (`IS` is null-safe).
        (drive_id, Some(charge_id)) => transaction.execute(
            "DELETE FROM lifecycle_open_rows WHERE source_id=?1 AND vehicle_id=?2
                AND NOT ((domain = 'position' AND parent_source_row_id IS ?3)
                      OR (domain = 'drive' AND source_row_id IS ?3)
                      OR (domain = 'charge_sample' AND parent_source_row_id IS ?4)
                      OR (domain = 'charge' AND source_row_id IS ?4))",
            params![source, vehicle, drive_id.unwrap_or(-1), charge_id],
        ),
    };
    cleared.map_err(StoreError::LifecycleWrite)?;
    // No watermark row of any domain is deleted (C2 R80 (b)): the writer below
    // only raises each pair, because a replacement or promotion must never
    // lower how far an imported class reaches (for `charging_processes`, C2
    // R5's import bound W(V)).
    let mut inserted = 0;
    if let Some(row) = session.drive.as_ref().filter(|_| kept_open.is_none()) {
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
    for row in session.drive_positions.iter().filter(|_| kept_open.is_none()) {
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
    if let Some(row) = session.charge.as_ref().filter(|_| kept_open_charge.is_none()) {
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
    for row in session
        .charge_samples
        .iter()
        .filter(|_| kept_open_charge.is_none())
    {
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
    let watermarks = snapshot_watermark_pairs(&handover_watermarks);
    let changed_domains =
        raise_source_watermarks_in_transaction(transaction, source_id, vehicle_id, &watermarks)?;
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
/// when the prior import did not list it, a Hub-range position belongs to it
/// or a recorded self-link marks it; every other stored source-id drive is an
/// earlier import's. The lifecycle's open drive the Hub owns, with its open
/// positions, is weighed too and settled here (M03-T18,
/// [`import_seam_hub_drives_in_connection`]). The pure
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
    use crate::db::hub_ids::is_hub_id;
    use crate::import::source_db::seam::{
        InheritedCopy, SeamLinks, SessionRow, SourceRows, plan_seam,
    };

    let vehicle_key = vehicle_id.to_string();
    let coverage = import_seam_coverage_in_transaction(transaction, &vehicle_key)?;
    // The same reader as the planner ahead of the packs (M03-T18).
    let SeamHubDrives {
        rows: hub,
        stored: stored_drives,
        hub_positions,
        open,
        hub_side,
        held_by_hub_position,
    } = import_seam_hub_drives_in_connection(
        transaction,
        &vehicle_key,
        &coverage,
        prior_imported_drive_ids,
    )?;
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

    // The lifecycle's open drive the Hub owns, decided like any Hub drive,
    // is settled before promotion can clear its open rows (M03-T18).
    if let Some(open) = &open {
        settle_import_seam_open_drive_in_transaction(transaction, vehicle_id, open, &plan)?;
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
    )?;
    // Inherited ownership that rests on nothing else durable is recorded, so
    // it holds once the import lists the id (M03-T18): a stored copy kept that
    // no Hub-range position holds. A mark goes when the source copy replaces
    // the row or the row goes stale.
    let kept: Vec<i64> = drive_plan
        .inherited
        .iter()
        .filter(|(id, copy)| {
            *copy == InheritedCopy::Stored
                && stored_drives.contains_key(id)
                && !held_by_hub_position.contains(id)
        })
        .map(|(id, _)| *id)
        .collect();
    let released: Vec<i64> = drive_plan
        .inherited
        .iter()
        .filter(|(_, copy)| *copy == InheritedCopy::Source)
        .map(|(id, _)| *id)
        .chain(drive_plan.stale.iter().copied())
        .collect();
    record_seam_inherited_in_transaction(transaction, &vehicle_key, "drive", &kept, &released)
}

/// Settles the lifecycle's open drive the Hub owns once the seam has decided
/// it (M03-T18). When it lost, or its source copy covers and replaces it (an
/// inherited session, one id), its Hub positions inside coverage are written
/// to `materialised_positions` under the drive the plan gives them (the
/// winner, or the source copy) and journalled; those outside coverage are
/// dropped; its open rows go and it leaves the lifecycle state, so neither
/// promotion nor a later collection closes it again. It was never synced, so
/// no tombstone is written; a lost drive's link is recorded by the caller.
/// When the plan keeps it, it stays open with its positions inside coverage,
/// and promotion keeps it; the lifecycle closes it when the Hub collects again.
fn settle_import_seam_open_drive_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    open: &SeamOpenDrive,
    plan: &crate::import::source_db::seam::SeamPlan,
) -> Result<(), StoreError> {
    use crate::db::hub_ids::{HUB_ID_BASE, HUB_ID_MAX};
    use crate::import::source_db::seam::InheritedCopy;

    let vehicle_key = vehicle_id.to_string();
    let drive_id = open.session.id;
    let drive_plan = &plan.drives;
    let removed = |id: i64| plan.positions.hub_removed.binary_search(&id).is_ok();
    let lost = drive_plan
        .superseded
        .iter()
        .chain(&drive_plan.skipped)
        .any(|link| link.loser_id == drive_id);
    let replaced = drive_plan
        .inherited
        .contains(&(drive_id, InheritedCopy::Source));
    if !lost && !replaced {
        for id in open.positions.keys().filter(|id| removed(**id)) {
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                      WHERE vehicle_id = ?1 AND domain = 'position' AND source_row_id = ?2",
                    params![vehicle_key.as_str(), id],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        return Ok(());
    }

    let moved: HashMap<i64, i64> = plan
        .positions
        .moved_hub
        .iter()
        .map(|moved| (moved.id, moved.to))
        .collect();
    let open_positions: Vec<(i64, i64, String)> = {
        let mut statement = transaction
            .prepare(
                "SELECT source_row_id, car_id, row_json FROM lifecycle_open_rows
                  WHERE vehicle_id = ?1 AND domain = 'position' AND parent_source_row_id = ?2
                    AND source_row_id BETWEEN ?3 AND ?4
                  ORDER BY source_row_id ASC",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(
                params![vehicle_key.as_str(), drive_id, HUB_ID_BASE, HUB_ID_MAX],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(StoreError::Query)?;
        let collected: Vec<(i64, i64, String)> =
            rows.collect::<Result<_, _>>().map_err(StoreError::Query)?;
        collected
    };
    for (id, car_id, row_json) in open_positions {
        if !open.positions.contains_key(&id) || removed(id) {
            continue;
        }
        let mut position: crate::hub_pack::ProjectionPosition =
            serde_json::from_str(&row_json).map_err(|_| StoreError::InvalidLifecycleSession)?;
        position.drive_id = if replaced {
            Some(drive_id)
        } else {
            moved.get(&id).copied()
        };
        insert_settled_hub_position_in_transaction(transaction, vehicle_id, car_id, &position)?;
    }
    transaction
        .execute(
            "DELETE FROM lifecycle_open_rows
              WHERE vehicle_id = ?1
                AND ((domain = 'position' AND parent_source_row_id = ?2)
                  OR (domain = 'drive' AND source_row_id = ?2))",
            params![vehicle_key.as_str(), drive_id],
        )
        .map_err(StoreError::LifecycleWrite)?;
    let Some(record) = load_lifecycle_state_in_transaction(transaction, vehicle_id)? else {
        return Ok(());
    };
    let mut state = crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
    if state.open_drive.as_ref().map(|drive| drive.id) != Some(drive_id) {
        return Ok(());
    }
    state.open_drive = None;
    if state.phase == crate::lifecycle::VehiclePhase::Driving {
        state.phase = if state.open_charge.is_some() {
            crate::lifecycle::VehiclePhase::Charging
        } else {
            crate::lifecycle::VehiclePhase::Online
        };
    }
    let json = state
        .encode()
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
    transaction
        .execute(
            "UPDATE vehicle_lifecycle_state SET open_session_json = ?2 WHERE vehicle_id = ?1",
            params![vehicle_key.as_str(), json],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

/// Writes one Hub position settled from an open drive and journals it
/// (M03-T18). A row already stored under the same id is left as it is.
fn insert_settled_hub_position_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    car_id: i64,
    position: &crate::hub_pack::ProjectionPosition,
) -> Result<(), StoreError> {
    let json = serde_json::to_string(position).map_err(StoreError::SerializeLifecycleRow)?;
    let inserted = transaction
        .execute(
            "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json,
                speed, power, est_battery_range_km, fan_status, driver_temp_setting, passenger_temp_setting,
                is_climate_on, is_rear_defroster_on, is_front_defroster_on, battery_heater, battery_heater_on,
                battery_heater_no_power, tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
             ON CONFLICT(vehicle_id, position_id) DO NOTHING",
            params![vehicle_id.to_string(), position.id, position.drive_id, car_id, json,
                position.speed, position.power, position.est_battery_range_km,
                position.fan_status, position.driver_temp_setting, position.passenger_temp_setting,
                position.is_climate_on.map(i64::from), position.is_rear_defroster_on.map(i64::from),
                position.is_front_defroster_on.map(i64::from), position.battery_heater.map(i64::from),
                position.battery_heater_on.map(i64::from), position.battery_heater_no_power.map(i64::from),
                position.tpms_pressure_fl, position.tpms_pressure_fr, position.tpms_pressure_rl, position.tpms_pressure_rr],
        )
        .map_err(StoreError::LifecycleWrite)?;
    if inserted == 0 {
        return Ok(());
    }
    record_sync_mutation_in_transaction(
        transaction,
        vehicle_id,
        "position",
        position.id,
        car_id,
        "upsert",
        &json,
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

/// Refuses source charges the seam cannot weigh: out of order, inside the
/// Hub's id range or of another car.
fn validate_import_seam_charges(
    charges: &[crate::hub_pack::ProjectionCharge],
    car_id: i64,
) -> Result<(), StoreError> {
    let mut previous_id = 0_i64;
    for charge in charges {
        if charge.id <= previous_id
            || charge.id >= crate::db::hub_ids::HUB_ID_BASE
            || charge.car_id != car_id
        {
            return Err(StoreError::InvalidImportGeneration);
        }
        previous_id = charge.id;
    }
    Ok(())
}

/// The source charges as sessions of the seam.
fn import_seam_source_charges(
    charges: &[crate::hub_pack::ProjectionCharge],
) -> Vec<crate::import::source_db::seam::SessionRow> {
    charges
        .iter()
        .map(|charge| crate::import::source_db::seam::SessionRow {
            id: charge.id,
            start_ms: charge.start_date_ms,
            end_ms: charge.end_date_ms,
        })
        .collect()
}

/// Reads the store's side of one vehicle's charges into `hub` (design
/// M03-SEAM-R3, section 4): the Hub-range charge samples by instant, and the
/// stored charges. Import never stores a source charge of its own, so a
/// stored charge with a source id is the Hub's inherited session (one id on
/// both sides) unless the prior import listed it, no Hub-range sample belongs
/// to it and no self-link marks it (M03-T18): then it is a source copy an
/// earlier plan wrote over the inherited row, and the stale rule applies to
/// it. When the vehicle has coverage, the lifecycle's open charge the Hub
/// owns takes part too, with its open samples
/// ([`import_seam_open_charge_in_connection`]). The one reader of both
/// planners. Reads only.
fn import_seam_hub_charges_in_connection(
    connection: &Connection,
    vehicle_key: &str,
    coverage: &crate::import::source_db::seam::HubCoverage,
    prior_imported_charge_ids: &HashSet<i64>,
    hub: &mut crate::import::source_db::seam::HubRows,
) -> Result<(), StoreError> {
    use crate::db::hub_ids::{HUB_ID_BASE, HUB_ID_MAX, is_hub_id};
    use crate::import::source_db::seam::{ChildRow, SessionRow};
    {
        let mut statement = connection
            .prepare(
                "SELECT sample_id, charge_id,
                        CAST(json_extract(sample_json, '$.timestamp_ms') AS INTEGER)
                   FROM materialised_charge_samples
                  WHERE vehicle_id = ?1 AND sample_id BETWEEN ?2 AND ?3
                  ORDER BY sample_id ASC",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![vehicle_key, HUB_ID_BASE, HUB_ID_MAX], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(StoreError::Query)?;
        for row in rows {
            let (id, parent, at_ms) = row.map_err(StoreError::Query)?;
            // A Hub sample without an instant cannot be placed; it stays.
            if let Some(at_ms) = at_ms {
                hub.charge_samples.push(ChildRow {
                    id,
                    at_ms,
                    parent: Some(parent),
                });
            }
        }
    }
    let held_by_hub_sample: HashSet<i64> = hub
        .charge_samples
        .iter()
        .filter_map(|row| row.parent)
        .collect();
    let marked = seam_inherited_in_connection(connection, vehicle_key, "charge")?;
    let mut statement = connection
        .prepare(
            "SELECT charge_id,
                    CAST(json_extract(charge_json, '$.start_date_ms') AS INTEGER),
                    CAST(json_extract(charge_json, '$.end_date_ms') AS INTEGER)
               FROM materialised_charges WHERE vehicle_id = ?1 ORDER BY charge_id ASC",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_key], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .map_err(StoreError::Query)?;
    for row in rows {
        let (id, start_ms, end_ms) = row.map_err(StoreError::Query)?;
        // A stored charge without a start cannot be weighed; it stays.
        let Some(start_ms) = start_ms else { continue };
        let inherited = !prior_imported_charge_ids.contains(&id)
            || held_by_hub_sample.contains(&id)
            || marked.contains(&id);
        if is_hub_id(id) || inherited {
            hub.charges.push(SessionRow {
                id,
                start_ms,
                end_ms,
            });
        } else {
            hub.imported_charges.push(id);
        }
    }
    if let Some(open) = import_seam_open_charge_in_connection(connection, vehicle_key, coverage)? {
        hub.charges.push(open.session);
        hub.charge_samples
            .extend(open.samples.iter().map(|(id, at_ms)| ChildRow {
                id: *id,
                at_ms: *at_ms,
                parent: Some(open.session.id),
            }));
    }
    Ok(())
}

/// The import seam applied to the charges and charge samples in the store
/// (design M03-SEAM-R3, section 4), from a plan made by
/// `import_seam_plan_in_transaction` in the same transaction.
///
/// Hub charges that lost are removed with a tombstone; a stored source-id
/// charge that was skipped or is stale goes the same way; Hub samples go by
/// instant (outside coverage removed with a tombstone, never because their
/// charge lost) and kept ones whose charge lost move to the winner; kept Hub
/// charges widen to hold the samples they gained; every loser is recorded in
/// `import_seam_links`. With `write_source_copies`, a winning source charge
/// whose id the store holds (an inherited session whose source copy now
/// covers the stored extent) replaces the stored row, journalled; import
/// writes no other source charge to the store. Applied again, the plan finds
/// its links and removed rows and changes nothing.
///
/// Publication (`write_source_copies`) also settles the lifecycle's open
/// charge the Hub owns ([`settle_import_seam_open_charge_in_transaction`]) and
/// records which inherited charges the Hub owns by decision (M03-T18). Ahead
/// of the packs neither happens: the removals and moves above look up
/// materialised rows only, so the open charge stays as the plan read it.
fn apply_import_seam_to_charges_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    plan: &crate::import::source_db::seam::SeamPlan,
    charges: &[crate::hub_pack::ProjectionCharge],
    write_source_copies: bool,
) -> Result<(), StoreError> {
    use crate::db::hub_ids::is_hub_id;
    use crate::import::source_db::seam::InheritedCopy;
    let vehicle_key = vehicle_id.to_string();
    let charge_plan = &plan.charges;
    let stored_charge = |id: i64| -> Result<Option<(i64, String)>, StoreError> {
        transaction
            .query_row(
                "SELECT car_id, charge_json FROM materialised_charges
                  WHERE vehicle_id = ?1 AND charge_id = ?2",
                params![vehicle_key.as_str(), id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)
    };
    let remove = |id: i64, car_id: i64| -> Result<(), StoreError> {
        transaction
            .execute(
                "DELETE FROM address_enrichment_jobs
                  WHERE vehicle_id = ?1 AND target_type = 'charge' AND target_id = ?2",
                params![vehicle_key.as_str(), id],
            )
            .map_err(StoreError::LifecycleWrite)?;
        transaction
            .execute(
                "DELETE FROM materialised_charges WHERE vehicle_id = ?1 AND charge_id = ?2",
                params![vehicle_key.as_str(), id],
            )
            .map_err(StoreError::LifecycleWrite)?;
        record_sync_mutation_in_transaction(
            transaction, vehicle_id, "charge", id, car_id, "tombstone", "{}",
        )
    };
    // Hub charges that lost. Their samples follow the instant rule below.
    for link in &charge_plan.superseded {
        if let Some((car_id, _)) = stored_charge(link.loser_id)? {
            remove(link.loser_id, car_id)?;
        }
    }
    // A stored source copy of a skipped or stale charge; never a Hub row.
    let source_gone = charge_plan
        .skipped
        .iter()
        .map(|link| link.loser_id)
        .chain(charge_plan.stale.iter().copied());
    for id in source_gone {
        if is_hub_id(id) {
            return Err(StoreError::InvalidImportGeneration);
        }
        if let Some((car_id, _)) = stored_charge(id)? {
            remove(id, car_id)?;
        }
    }
    let widened: HashMap<i64, (i64, Option<i64>)> = charge_plan
        .widened
        .iter()
        .map(|bounds| (bounds.id, (bounds.start_ms, bounds.end_ms)))
        .collect();
    // The source copy of an inherited charge replaces the stored row.
    if write_source_copies {
        let by_id: HashMap<i64, &crate::hub_pack::ProjectionCharge> =
            charges.iter().map(|charge| (charge.id, charge)).collect();
        for id in &charge_plan.imported {
            let Some(charge) = by_id.get(id) else {
                return Err(StoreError::InvalidImportGeneration);
            };
            let Some((_, stored_json)) = stored_charge(*id)? else {
                continue;
            };
            let planned = planned_import_charge(charge, widened.get(id).copied());
            let charge_json =
                serde_json::to_string(&planned).map_err(StoreError::SerializeLifecycleRow)?;
            if charge_json == stored_json {
                continue;
            }
            transaction
                .execute(
                    "UPDATE materialised_charges SET car_id = ?3, charge_json = ?4
                      WHERE vehicle_id = ?1 AND charge_id = ?2",
                    params![vehicle_key.as_str(), id, planned.car_id, charge_json],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                vehicle_id,
                "charge",
                *id,
                planned.car_id,
                "upsert",
                &charge_json,
            )?;
        }
    }
    // Kept Hub charges widen to hold the samples they gained; totals stay.
    for id in &charge_plan.hub_kept {
        let Some((start_ms, end_ms)) = widened.get(id).copied() else {
            continue;
        };
        let Some((car_id, charge_json)) = stored_charge(*id)? else {
            continue;
        };
        let mut value: Value =
            serde_json::from_str(&charge_json).map_err(|_| StoreError::InvalidLifecycleSession)?;
        let Some(object) = value.as_object_mut() else {
            return Err(StoreError::InvalidLifecycleSession);
        };
        object.insert("start_date_ms".into(), Value::from(start_ms));
        if let Some(end_ms) = end_ms {
            object.insert("end_date_ms".into(), Value::from(end_ms));
        }
        let widened_json = value.to_string();
        if widened_json == charge_json {
            continue;
        }
        transaction
            .execute(
                "UPDATE materialised_charges SET charge_json = ?3
                  WHERE vehicle_id = ?1 AND charge_id = ?2",
                params![vehicle_key.as_str(), id, widened_json],
            )
            .map_err(StoreError::LifecycleWrite)?;
        record_sync_mutation_in_transaction(
            transaction, vehicle_id, "charge", *id, car_id, "upsert", &widened_json,
        )?;
    }
    // Hub samples by instant: outside coverage they go with a tombstone.
    // A sample row holds no car, so its tombstone names its charge's car.
    let sample_car = |id: i64| -> Result<Option<i64>, StoreError> {
        transaction
            .query_row(
                "SELECT COALESCE(
                    (SELECT car_id FROM materialised_charges AS charge
                      WHERE charge.vehicle_id = sample.vehicle_id
                        AND charge.charge_id = sample.charge_id),
                    (SELECT car_id FROM materialised_cars AS car
                      WHERE car.vehicle_id = sample.vehicle_id))
                   FROM materialised_charge_samples AS sample
                  WHERE sample.vehicle_id = ?1 AND sample.sample_id = ?2",
                params![vehicle_key.as_str(), id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(StoreError::Query)
            .map(Option::flatten)
    };
    for id in &plan.charge_samples.hub_removed {
        let Some(car_id) = sample_car(*id)? else {
            continue;
        };
        transaction
            .execute(
                "DELETE FROM materialised_charge_samples WHERE vehicle_id = ?1 AND sample_id = ?2",
                params![vehicle_key.as_str(), id],
            )
            .map_err(StoreError::LifecycleWrite)?;
        record_sync_mutation_in_transaction(
            transaction, vehicle_id, "charge_sample", *id, car_id, "tombstone", "{}",
        )?;
    }
    // Kept Hub samples whose charge lost move to the winner.
    for moved in &plan.charge_samples.moved_hub {
        let Some(car_id) = sample_car(moved.id)? else {
            continue;
        };
        let sample_json: String = transaction
            .query_row(
                "SELECT sample_json FROM materialised_charge_samples
                  WHERE vehicle_id = ?1 AND sample_id = ?2",
                params![vehicle_key.as_str(), moved.id],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let mut value: Value =
            serde_json::from_str(&sample_json).map_err(|_| StoreError::InvalidLifecycleSession)?;
        let Some(object) = value.as_object_mut() else {
            return Err(StoreError::InvalidLifecycleSession);
        };
        object.insert("charge_process_id".into(), Value::from(moved.to));
        let moved_json = value.to_string();
        transaction
            .execute(
                "UPDATE materialised_charge_samples SET charge_id = ?3, sample_json = ?4
                  WHERE vehicle_id = ?1 AND sample_id = ?2",
                params![vehicle_key.as_str(), moved.id, moved.to, moved_json],
            )
            .map_err(StoreError::LifecycleWrite)?;
        record_sync_mutation_in_transaction(
            transaction,
            vehicle_id,
            "charge_sample",
            moved.id,
            car_id,
            "upsert",
            &moved_json,
        )?;
    }
    // The lifecycle's open charge the Hub owns, decided like any Hub charge,
    // is settled before promotion can clear its open rows (M03-T18).
    if write_source_copies {
        let coverage = import_seam_coverage_in_transaction(transaction, &vehicle_key)?;
        if let Some(open) =
            import_seam_open_charge_in_connection(transaction, &vehicle_key, &coverage)?
        {
            settle_import_seam_open_charge_in_transaction(transaction, vehicle_id, &open, plan)?;
        }
    }
    let decided: Vec<_> = charge_plan
        .skipped
        .iter()
        .chain(&charge_plan.superseded)
        .copied()
        .collect();
    record_seam_links_in_transaction(
        transaction,
        &vehicle_key,
        "charge",
        &decided,
        &charge_plan.links_dropped,
    )?;
    if !write_source_copies {
        return Ok(());
    }
    // Inherited ownership that rests on nothing else durable is recorded, so
    // it holds once the import lists the id (M03-T18): a stored copy kept that
    // no Hub-range sample holds once the plan is applied. A mark goes when the
    // source copy replaces the row or the row goes stale.
    let held_by_hub_sample = |id: i64| -> Result<bool, StoreError> {
        transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM materialised_charge_samples
                                WHERE vehicle_id = ?1 AND charge_id = ?2
                                  AND sample_id BETWEEN ?3 AND ?4)",
                params![
                    vehicle_key.as_str(),
                    id,
                    crate::db::hub_ids::HUB_ID_BASE,
                    crate::db::hub_ids::HUB_ID_MAX
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    };
    let mut kept = Vec::new();
    let mut released = Vec::new();
    for (id, copy) in &charge_plan.inherited {
        match copy {
            InheritedCopy::Stored => {
                if stored_charge(*id)?.is_some() && !held_by_hub_sample(*id)? {
                    kept.push(*id);
                }
            }
            InheritedCopy::Source => released.push(*id),
        }
    }
    released.extend(charge_plan.stale.iter().copied());
    record_seam_inherited_in_transaction(transaction, &vehicle_key, "charge", &kept, &released)
}

/// Settles the lifecycle's open charge the Hub owns once the seam has decided
/// it (M03-T18, second part), as the open drive is settled. When it lost, or
/// its source copy covers and replaces it (an inherited session, one id), its
/// Hub samples inside coverage are written to `materialised_charge_samples`
/// under the charge the plan gives them (the winner, or the same id) and
/// journalled; those outside coverage are dropped; its open rows go and it
/// leaves the lifecycle state, so neither promotion nor a later collection
/// closes it again. It was never synced, so no tombstone is written; a lost
/// charge's link is recorded by the caller. When the plan keeps it, it stays
/// open with its samples inside coverage, and promotion keeps it.
fn settle_import_seam_open_charge_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    open: &SeamOpenCharge,
    plan: &crate::import::source_db::seam::SeamPlan,
) -> Result<(), StoreError> {
    use crate::db::hub_ids::{HUB_ID_BASE, HUB_ID_MAX};
    use crate::import::source_db::seam::InheritedCopy;

    let vehicle_key = vehicle_id.to_string();
    let charge_id = open.session.id;
    let charge_plan = &plan.charges;
    let removed: HashSet<i64> = plan.charge_samples.hub_removed.iter().copied().collect();
    let link = charge_plan
        .superseded
        .iter()
        .chain(&charge_plan.skipped)
        .find(|link| link.loser_id == charge_id);
    let replaced = charge_plan
        .inherited
        .contains(&(charge_id, InheritedCopy::Source));
    if link.is_none() && !replaced {
        for id in open.samples.keys().filter(|id| removed.contains(id)) {
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                      WHERE vehicle_id = ?1 AND domain = 'charge_sample' AND source_row_id = ?2",
                    params![vehicle_key.as_str(), id],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        return Ok(());
    }

    // A kept sample of a charge that lost moves where the plan moves it, or
    // to the charge's recorded winner; one of a replaced charge stays under
    // the same id.
    let planned_charge = link.map_or(charge_id, |link| link.winner_id);
    let moved: HashMap<i64, i64> = plan
        .charge_samples
        .moved_hub
        .iter()
        .map(|moved| (moved.id, moved.to))
        .collect();
    let open_samples: Vec<(i64, i64, String)> = {
        let mut statement = transaction
            .prepare(
                "SELECT source_row_id, car_id, row_json FROM lifecycle_open_rows
                  WHERE vehicle_id = ?1 AND domain = 'charge_sample' AND parent_source_row_id = ?2
                    AND source_row_id BETWEEN ?3 AND ?4
                  ORDER BY source_row_id ASC",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(
                params![vehicle_key.as_str(), charge_id, HUB_ID_BASE, HUB_ID_MAX],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(StoreError::Query)?;
        let collected: Vec<(i64, i64, String)> =
            rows.collect::<Result<_, _>>().map_err(StoreError::Query)?;
        collected
    };
    for (id, car_id, row_json) in open_samples {
        if !open.samples.contains_key(&id) || removed.contains(&id) {
            continue;
        }
        let mut sample: crate::hub_pack::ProjectionChargeSample =
            serde_json::from_str(&row_json).map_err(|_| StoreError::InvalidLifecycleSession)?;
        sample.charge_process_id = moved.get(&id).copied().unwrap_or(planned_charge);
        insert_settled_hub_sample_in_transaction(transaction, vehicle_id, car_id, &sample)?;
    }
    transaction
        .execute(
            "DELETE FROM lifecycle_open_rows
              WHERE vehicle_id = ?1
                AND ((domain = 'charge_sample' AND parent_source_row_id = ?2)
                  OR (domain = 'charge' AND source_row_id = ?2))",
            params![vehicle_key.as_str(), charge_id],
        )
        .map_err(StoreError::LifecycleWrite)?;
    let Some(record) = load_lifecycle_state_in_transaction(transaction, vehicle_id)? else {
        return Ok(());
    };
    let mut state = crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
    if state.open_charge.as_ref().map(|charge| charge.id) != Some(charge_id) {
        return Ok(());
    }
    state.open_charge = None;
    if state.phase == crate::lifecycle::VehiclePhase::Charging {
        state.phase = if state.open_drive.is_some() {
            crate::lifecycle::VehiclePhase::Driving
        } else {
            crate::lifecycle::VehiclePhase::Online
        };
    }
    let json = state
        .encode()
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
    transaction
        .execute(
            "UPDATE vehicle_lifecycle_state SET open_session_json = ?2 WHERE vehicle_id = ?1",
            params![vehicle_key.as_str(), json],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

/// Writes one Hub charge sample settled from an open charge and journals it
/// (M03-T18). A row already stored under the same id is left as it is.
fn insert_settled_hub_sample_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    car_id: i64,
    sample: &crate::hub_pack::ProjectionChargeSample,
) -> Result<(), StoreError> {
    let json = serde_json::to_string(sample).map_err(StoreError::SerializeLifecycleRow)?;
    let inserted = transaction
        .execute(
            "INSERT INTO materialised_charge_samples(vehicle_id, sample_id, charge_id, sample_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(vehicle_id, sample_id) DO NOTHING",
            params![vehicle_id.to_string(), sample.id, sample.charge_process_id, json],
        )
        .map_err(StoreError::LifecycleWrite)?;
    if inserted == 0 {
        return Ok(());
    }
    record_sync_mutation_in_transaction(
        transaction,
        vehicle_id,
        "charge_sample",
        sample.id,
        car_id,
        "upsert",
        &json,
    )
}

/// The row a packed source charge is written as: its source copy with the
/// bounds the plan widened it to.
pub(crate) fn planned_import_charge(
    charge: &crate::hub_pack::ProjectionCharge,
    bounds: Option<(i64, Option<i64>)>,
) -> crate::hub_pack::ProjectionCharge {
    let mut planned = charge.clone();
    if let Some((start_ms, end_ms)) = bounds {
        planned.start_date_ms = start_ms;
        if end_ms.is_some() {
            planned.end_date_ms = end_ms;
        }
    }
    planned
}
