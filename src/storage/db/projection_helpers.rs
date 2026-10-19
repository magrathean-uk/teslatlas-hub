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

/// Version-2 key of an `address_lookup_cache` row:
/// `v2:` + unpadded URL-safe base64 of the SHA-256 of four length-prefixed
/// components in this order: the normalised endpoint, the language as sent,
/// the latitude and the longitude, both formatted `{:.6}`. Each component is
/// its byte length as eight big-endian bytes followed by its bytes, so no two
/// different tuples can produce the same input. The key is 46 bytes, inside
/// the column's 64-byte CHECK. Version-1 keys (`{lat:.6}:{lon:.6}`, written
/// before the key bound a provider) are never produced again and are not read.
fn address_lookup_key(
    binding: &AddressLookupBinding,
    point: crate::location::Wgs84Point,
) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let latitude = format!("{:.6}", point.latitude);
    let longitude = format!("{:.6}", point.longitude);
    let mut digest = Sha256::new();
    for component in [
        binding.endpoint.as_str(),
        binding.language.as_str(),
        latitude.as_str(),
        longitude.as_str(),
    ] {
        digest.update((component.len() as u64).to_be_bytes());
        digest.update(component.as_bytes());
    }
    format!("v2:{}", URL_SAFE_NO_PAD.encode(digest.finalize()))
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

/// The source kind the importer registers, the only kind whose
/// `charging_processes` watermark marks charge rows as imported (C2 R5).
const IMPORT_SOURCE_KIND: &str = "teslamate";

/// The `lifecycle_source_watermarks.domain` the imported-session seeding
/// writes for the imported charge rows of a vehicle (C2 R5, section 12 item
/// 6): the source table name.
const CHARGING_PROCESSES_DOMAIN: &str = "charging_processes";

/// Which charge rows of one vehicle are imported (C2 R5): those with
/// `charge_id <= max_imported_charge_id`, the vehicle's `W(V)`; zero means
/// none. Every other row is a Hub row.
///
/// The test is per row, as C9 R6 is for positions, because one vehicle holds
/// imported and live rows together (seeding links the live vehicle to the
/// import source and the imported open process continues live in it), so the
/// vehicle's own source says nothing about a row. The id alone is exact
/// because C1 R10 with amendment A5 allocates every live charge id above
/// `W(V)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChargeImportBound {
    max_imported_charge_id: i64,
}

impl ChargeImportBound {
    /// True when the charge row with this id was imported (C2 R5).
    fn is_imported(&self, charge_id: i64) -> bool {
        charge_id <= self.max_imported_charge_id
    }
}

/// `W(V)` of C2 R5: the largest `max_source_row_id` over the vehicle's
/// `charging_processes` watermarks of sources of the import kind, 0 when there
/// is none. Reads only `sources` and `lifecycle_source_watermarks`: neither
/// the vehicle's own source nor the lifecycle state blob is consulted, because
/// a vehicle's source says nothing about one row and missing lifecycle state
/// must not turn into "everything is Hub-collected".
fn charge_import_bound(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<ChargeImportBound, StoreError> {
    let largest: Option<i64> = connection
        .query_row(
            "SELECT MAX(w.max_source_row_id)
             FROM lifecycle_source_watermarks w
             JOIN sources s ON s.source_id = w.source_id
             WHERE w.vehicle_id = ?1 AND s.source_kind = ?2 AND w.domain = ?3",
            params![
                vehicle_id.to_string(),
                IMPORT_SOURCE_KIND,
                CHARGING_PROCESSES_DOMAIN
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    Ok(ChargeImportBound {
        max_imported_charge_id: largest.unwrap_or(0).max(0),
    })
}

/// Most newest charge rows one efficiency recomputation reads (C2 R63). 200
/// rows leave room for road trips with many DC stops, whose rows are read and
/// skipped, while bounding the work of every charge close.
const K_EFF_SCAN_ROWS: i64 = 200;

/// Most candidate sessions one efficiency recomputation uses (C2 R63). 20
/// sessions follow a firmware change of the car's range constant within weeks
/// of daily charging or months of weekly charging, while bounding the number
/// of sample sets loaded at every close.
const K_EFF_CANDIDATES: usize = 20;

/// Rows of one charge's samples the efficiency recomputation reads at once
/// (C2 R71): 1,000.
///
/// (1) Memory: a stored sample is about 0.6 KiB of JSON (22 named fields; 578
/// bytes with every field set), so a page holds about 0.6 MiB of row text in
/// SQLite's sorter, a fixed amount far inside the 200 MiB steady budget
/// whatever the session's length. (2) Work: each page query walks every index
/// entry of the charge and evaluates each row's time once, so a session of n
/// samples costs about n x (floor(n / 1,000) + 1) row visits; a session of up
/// to 999 samples (8 h 19 min at the Owner wire's 30 s charging cadence, 33 h
/// at Fleet polling's 120 s) is read by one query, and extra visits arise only
/// for longer sessions. (3) It equals the read API's own default charge-sample
/// page, so the Hub's two readers of one charge's samples have one page bound.
const K_EFF_SAMPLE_PAGE: i64 = 1_000;

/// The result of reading one session's samples for its efficiency figure
/// (C2 R71 (c)).
#[derive(Debug, Clone, Copy, PartialEq)]
struct SessionEfficiencyRead {
    /// The session's figure, or `None` (R62), or when `undecodable`.
    figure: Option<f64>,
    /// A stored sample did not decode, had a decoded id other than its key, or
    /// had no time (R63: the session then gives no figure).
    undecodable: bool,
    /// Rows placed into the fold.
    rows: u64,
    /// Page queries run.
    queries: u64,
    /// The largest number of rows one query returned.
    largest_page: u64,
}

/// One keyset page of a charge's samples in sample order, written as the read
/// API's page query writes it.
const EFFICIENCY_SAMPLE_PAGE_SQL: &str = "SELECT
        CAST(json_extract(sample_json, '$.timestamp_ms') AS INTEGER), sample_id, sample_json
     FROM materialised_charge_samples
     WHERE vehicle_id = ?1 AND charge_id = ?2
       AND (CAST(json_extract(sample_json, '$.timestamp_ms') AS INTEGER), sample_id) > (?3, ?4)
     ORDER BY CAST(json_extract(sample_json, '$.timestamp_ms') AS INTEGER) ASC, sample_id ASC
     LIMIT ?5";

/// Read one candidate session's samples in sample order, `page_rows` at a time,
/// through the fixed-size fold of [`crate::lifecycle::EfficiencyFold`] (C2 R71).
/// A page is folded and dropped before the next is read, so the memory used
/// does not grow with the session. SQL errors still fail. `page_rows >= 1`;
/// production passes [`K_EFF_SAMPLE_PAGE`] and tests pass smaller values.
fn read_session_efficiency(
    connection: &Connection,
    vehicle_id: Uuid,
    charge_id: i64,
    page_rows: i64,
) -> Result<SessionEfficiencyRead, StoreError> {
    let vehicle_key = vehicle_id.to_string();
    let counted: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM materialised_charge_samples
             WHERE vehicle_id = ?1 AND charge_id = ?2",
            params![vehicle_key, charge_id],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    let mut statement = connection
        .prepare(EFFICIENCY_SAMPLE_PAGE_SQL)
        .map_err(StoreError::Query)?;
    let limit = page_rows.max(1);
    let mut fold = crate::lifecycle::EfficiencyFold::new();
    let mut read = SessionEfficiencyRead {
        figure: None,
        undecodable: false,
        rows: 0,
        queries: 0,
        largest_page: 0,
    };
    let mut keyset = (i64::MIN, 0_i64);
    'pages: loop {
        // The page's rows go to the fold one at a time, as SQLite returns them (R71
        // (c) step 3: each row goes to `observe` and is dropped), so no second copy
        // of the page's row text is held beside the sorter's.
        let mapped = statement
            .query_map(
                params![vehicle_key, charge_id, keyset.0, keyset.1, limit],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(StoreError::Query)?;
        read.queries += 1;
        let mut in_page: i64 = 0;
        for mapped_row in mapped {
            let (time, sample_id, json) = mapped_row.map_err(StoreError::Query)?;
            in_page += 1;
            read.largest_page = read.largest_page.max(in_page as u64);
            read.rows += 1;
            let decoded = serde_json::from_str::<crate::hub_pack::ProjectionChargeSample>(&json);
            let Ok(sample) = decoded else {
                read.undecodable = true;
                break 'pages;
            };
            if sample.id != sample_id {
                read.undecodable = true;
                break 'pages;
            }
            fold.observe(&sample);
            keyset = (time.unwrap_or(i64::MIN), sample_id);
        }
        if in_page < limit {
            break;
        }
    }
    // A row without a time is never admitted by the keyset comparison, so the
    // rows the pages placed fall short of the rows counted: the session is
    // undecodable, as the whole-session reader would have found it.
    if !read.undecodable && read.rows != counted as u64 {
        read.undecodable = true;
    }
    read.figure = if read.undecodable { None } else { fold.finish() };
    Ok(read)
}

/// Recompute the car's efficiency from its closed Hub charges (C2 R63 to R67)
/// and write it only when it changes.
///
/// 1. The newest [`K_EFF_SCAN_ROWS`] rows of `materialised_charges` for
///    `(vehicle_id, car_id)`, by charge id descending. A row is a candidate
///    when it is a Hub row (R5: not at or below the import bound), closed and
///    `is_dc == Some(false)`; the scan stops after [`K_EFF_CANDIDATES`]
///    candidates. A DC or unknown kind is excluded because the battery-side
///    energy of a DC session overstates stored energy by several per cent
///    (current 10 to 30 times AC, resistive loss with its square), and an
///    unknown kind is excluded rather than guessed.
/// 2. Each candidate's samples are read in pages of [`K_EFF_SAMPLE_PAGE`]
///    rows through the fixed-size fold of [`crate::lifecycle::EfficiencyFold`]
///    (R71), which gives [`crate::lifecycle::session_efficiency_wh_per_km`]'s
///    figure bit for bit without holding the session.
/// 3. [`crate::lifecycle::car_efficiency_from_figures`] gives the car value, or
///    nothing below three figures, in which case the stored value (imported,
///    earlier or `None`) stays (R65, R68).
/// 4. R66: the car row of `(vehicle_id, car_id)`, when it exists and holds a
///    different value, gets the new `car_json` and one `car` sync mutation;
///    otherwise nothing is written. With no car row nothing is done.
///
/// A row or sample that does not decode is skipped, counted and logged once
/// as a count (R7, R63): one undecodable stored row must not roll back every
/// later charge close and block intake for the vehicle on every retry. SQL
/// errors still fail. The result is a pure function of stored rows and
/// samples (R69), so a second call, a replay or a restart gives the same
/// `car_json` and no second mutation. Called at a close (R67, after the
/// closing rows and samples are materialised) and by any path that deletes or
/// corrects a charge or its samples (R67a).
fn recompute_car_efficiency(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    car_id: i64,
) -> Result<(), StoreError> {
    let vehicle_key = vehicle_id.to_string();
    let car_json: Option<String> = transaction
        .query_row(
            "SELECT car_json FROM materialised_cars
             WHERE vehicle_id = ?1 AND car_id = ?2",
            params![vehicle_key, car_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    let Some(car_json) = car_json else {
        return Ok(());
    };
    let bound = charge_import_bound(transaction, vehicle_id)?;
    let mut statement = transaction
        .prepare(
            "SELECT charge_id, charge_json FROM materialised_charges
             WHERE vehicle_id = ?1 AND car_id = ?2
             ORDER BY charge_id DESC LIMIT ?3",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_key, car_id, K_EFF_SCAN_ROWS], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(StoreError::Query)?;
    let mut figures = Vec::new();
    let mut candidates = 0_usize;
    let mut skipped = 0_u64;
    for row in rows {
        let (charge_id, charge_json) = row.map_err(StoreError::Query)?;
        if bound.is_imported(charge_id) {
            continue;
        }
        let Ok(charge) = serde_json::from_str::<crate::hub_pack::ProjectionCharge>(&charge_json)
        else {
            skipped += 1;
            continue;
        };
        if charge.end_date_ms.is_none() || charge.is_dc != Some(false) {
            continue;
        }
        candidates += 1;
        let read = read_session_efficiency(transaction, vehicle_id, charge_id, K_EFF_SAMPLE_PAGE)?;
        if read.undecodable {
            skipped += 1;
        } else {
            figures.extend(read.figure);
        }
        if candidates >= K_EFF_CANDIDATES {
            break;
        }
    }
    drop(statement);
    if skipped > 0 {
        // R7: the vehicle id and a count only.
        tracing::warn!(
            vehicle_id = %vehicle_id,
            skipped,
            "undecodable charge rows or samples skipped by the efficiency recomputation"
        );
    }
    let Some(value) = crate::lifecycle::car_efficiency_from_figures(&figures) else {
        return Ok(());
    };
    let mut car: crate::hub_pack::ProjectionCar =
        serde_json::from_str(&car_json).map_err(StoreError::DeserializeLifecycleRow)?;
    if car.efficiency_wh_per_km == Some(value) {
        return Ok(());
    }
    car.efficiency_wh_per_km = Some(value);
    let car_json = serde_json::to_string(&car).map_err(StoreError::SerializeLifecycleRow)?;
    transaction
        .execute(
            "UPDATE materialised_cars SET car_json = ?3
             WHERE vehicle_id = ?1 AND car_id = ?2",
            params![vehicle_key, car_id, car_json],
        )
        .map_err(StoreError::LifecycleWrite)?;
    record_sync_mutation_in_transaction(
        transaction,
        vehicle_id,
        "car",
        car_id,
        car_id,
        "upsert",
        &car_json,
    )
}

/// Recompute a drive's ascent and descent after terrain elevation arrives later
/// (C1 R52, R85).
///
/// It calls the one shared function, [`crate::lifecycle::elevation_totals`],
/// over the drive's materialised positions in `(date_ms, id)` order, the order
/// the live fold emits them in. It writes only that drive's
/// `materialised_drives` row, and only `ascent`, `descent` and `drive_json`
/// (the stored row JSON with only its `ascent` and `descent` members
/// replaced). It writes no other column, row or table and records no
/// `sync_mutations` row: its caller records the change.
fn recompute_terrain_drive(
    transaction: &Transaction<'_>,
    vehicle_id: &str,
    drive_id: i64,
) -> Result<(), StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT position_json FROM materialised_positions
             WHERE vehicle_id = ?1 AND drive_id = ?2",
        )
        .map_err(StoreError::LifecycleWrite)?;
    let rows = statement
        .query_map(params![vehicle_id, drive_id], |row| row.get::<_, String>(0))
        .map_err(StoreError::LifecycleWrite)?;
    let mut positions: Vec<ProjectionPosition> = rows
        .map(|row| {
            let json = row.map_err(StoreError::LifecycleWrite)?;
            serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)
        })
        .collect::<Result<_, _>>()?;
    // The shared walk reads the positions in `(date_ms, id)` order, the order
    // the live fold emits them in (C1 R51, R52); the table has no date column.
    positions.sort_by_key(|position| (position.date_ms, position.id));
    let (ascent, descent) =
        crate::lifecycle::elevation_totals(positions.iter().map(|position| position.elevation));
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
    drive.ascent = ascent;
    drive.descent = descent;
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
        || !record.binding.is_canonical()
        || record.licence.as_deref().is_some_and(|licence| {
            licence.is_empty()
                || licence.len() > MAX_ADDRESS_LICENCE_BYTES
                || licence.chars().any(char::is_control)
        })
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

/// The largest `max_source_row_id` of each import domain over every
/// `lifecycle_source_watermarks` row of the vehicle, whichever source wrote it
/// (C1 R84); NULL counts as 0. Rows of every source count, not only the
/// import kind: a cursor that is too high only skips unused values, while one
/// that is too low aliases rows.
fn watermark_id_floors(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<crate::lifecycle::IdFloors, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT domain, MAX(COALESCE(max_source_row_id, 0))
             FROM lifecycle_source_watermarks
             WHERE vehicle_id = ?1 GROUP BY domain",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(StoreError::Query)?;
    let mut floors = crate::lifecycle::IdFloors::default();
    for row in rows {
        let (domain, largest) = row.map_err(StoreError::Query)?;
        match domain.as_str() {
            "drives" => floors.drives = largest,
            "positions" => floors.positions = largest,
            "charging_processes" => floors.charges = largest,
            "charges" => floors.charge_samples = largest,
            "states" => floors.states = largest,
            "updates" => floors.updates = largest,
            _ => {}
        }
    }
    Ok(floors)
}

/// Raise a state's id cursors above the vehicle's import watermarks as they
/// stand in this transaction (C1 R84 point (b)). It runs whether or not the
/// state was seeded before.
fn raise_id_cursors_above_watermarks(
    connection: &Connection,
    vehicle_id: Uuid,
    state: &mut crate::lifecycle::OpenSessionState,
) -> Result<(), StoreError> {
    let floors = watermark_id_floors(connection, vehicle_id)?;
    state.raise_id_cursors(&floors);
    Ok(())
}

/// Seed the installed version of a freshly seeded lifecycle state from the
/// newest imported update that has a comparable version (C1 R14, section 7
/// item 9), so an update installed between the source's last record and live
/// collection is recorded instead of silently adopted. Both seeding paths call
/// this after the session's own watermark rows are written in the same
/// transaction (C1 R89).
fn seed_imported_version_from_updates(
    connection: &Connection,
    vehicle_id: Uuid,
    state: &mut crate::lifecycle::OpenSessionState,
) -> Result<(), StoreError> {
    if state.last_car_version.is_some() {
        return Ok(());
    }
    let mut statement = connection
        .prepare(
            "SELECT update_json FROM materialised_updates
             WHERE vehicle_id = ?1 ORDER BY update_id ASC",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_id.to_string()], |row| {
            row.get::<_, String>(0)
        })
        .map_err(StoreError::Query)?;
    let mut updates = Vec::new();
    for row in rows {
        let json = row.map_err(StoreError::Query)?;
        updates.push(
            serde_json::from_str::<crate::hub_pack::ProjectionUpdate>(&json)
                .map_err(|_| StoreError::InvalidLifecycleSession)?,
        );
    }
    crate::lifecycle::seed_version_from_updates(state, &updates);
    if state.last_car_version.is_some() {
        return Ok(());
    }
    // Import finalisation materialises the car and its drives, and leaves the
    // newest imported update's version on the car row; the update rows stay in
    // the import projection and its packs, which this seed cannot read. The
    // version is that text, and its time is a lower bound of when the source
    // last saw it installed (C1 R89).
    let version = imported_car_firmware(connection, vehicle_id)?;
    if version.is_none() {
        return Ok(());
    }
    let known_since_ms = imported_version_known_since_ms(connection, vehicle_id)?;
    crate::lifecycle::seed_version_from_car_row(state, version.as_deref(), known_since_ms);
    Ok(())
}

/// The time a version seeded from the car row is known to have been installed
/// by (C1 R89 step 2): the greatest non-null `max_timestamp_ms` among the
/// vehicle's `updates` watermark rows, whichever source wrote them. That is the
/// import's own record of how far its updates class reached, so it belongs to
/// the same imported class as the version and is a true lower bound even when
/// the source missed a later update. Only when the import recorded no time for
/// its updates class is it the greatest end of the vehicle's materialised
/// drives (known limit 12); a value that is not positive counts as none.
fn imported_version_known_since_ms(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<Option<i64>, StoreError> {
    let vehicle_key = vehicle_id.to_string();
    let watermark: Option<i64> = connection
        .query_row(
            "SELECT MAX(max_timestamp_ms) FROM lifecycle_source_watermarks
             WHERE vehicle_id = ?1 AND domain = 'updates'",
            params![vehicle_key],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    if let Some(at) = watermark.filter(|at| *at > 0) {
        return Ok(Some(at));
    }
    let drive_end: Option<i64> = connection
        .query_row(
            "SELECT MAX(json_extract(drive_json, '$.end_date_ms'))
             FROM materialised_drives WHERE vehicle_id = ?1",
            params![vehicle_key],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    Ok(drive_end.filter(|at| *at > 0))
}

/// The `firmware_version` of the vehicle's materialised car row, when it has
/// one.
fn imported_car_firmware(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<Option<String>, StoreError> {
    let car_json: Option<String> = connection
        .query_row(
            "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    Ok(car_json
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|car| {
            car.get("firmware_version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        }))
}

fn load_geofence_fences(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<Vec<crate::lifecycle::GeofenceFence>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT source_geofence_id, name, latitude, longitude, radius_m, billing_type,
                    cost_per_unit, session_fee
             FROM geofences WHERE vehicle_id = ?1 ORDER BY source_geofence_id",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_id.to_string()], |row| {
            Ok(crate::lifecycle::GeofenceFence {
                source_geofence_id: row.get(0)?,
                name: row.get(1)?,
                latitude: row.get(2)?,
                longitude: row.get(3)?,
                radius_m: row.get(4)?,
                billing_type: row
                    .get::<_, Option<String>>(5)?
                    .map(|value| match value.as_str() {
                        "per_kwh" => crate::hub_pack::GeofenceBillingType::PerKwh,
                        "per_minute" => crate::hub_pack::GeofenceBillingType::PerMinute,
                        _ => crate::hub_pack::GeofenceBillingType::PerKwh,
                    }),
                cost_per_unit: row.get(6)?,
                session_fee: row.get(7)?,
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
            "SELECT source_geofence_id, name, latitude, longitude, radius_m, billing_type,
                    cost_per_unit, session_fee
             FROM geofences WHERE vehicle_id = ?1 AND source_geofence_id = ?2",
            params![vehicle_id.to_string(), source_geofence_id],
            |row| {
                Ok(crate::lifecycle::GeofenceFence {
                    source_geofence_id: row.get(0)?,
                    name: row.get(1)?,
                    latitude: row.get(2)?,
                    longitude: row.get(3)?,
                    radius_m: row.get(4)?,
                    billing_type: row
                        .get::<_, Option<String>>(5)?
                        .and_then(|value| value.parse().ok()),
                    cost_per_unit: row.get(6)?,
                    session_fee: row.get(7)?,
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

/// The cost-rule facts of a stored charge's samples (C2 R43a, R44a), as the
/// fill (R50) and manual entry (R54) read them: `materialised_charge_samples`
/// through the `(vehicle_id, charge_id)` index, folded one row at a time in any
/// order, so the memory of the step does not grow with the length of the
/// charge and no sorted copy is made. A row that does not decode is an error
/// here: a fill or a manual entry must not price a row from a partial view of
/// its samples.
fn fold_materialised_charge_samples(
    connection: &Connection,
    vehicle_id: Uuid,
    charge_id: i64,
) -> Result<crate::lifecycle::ChargeSampleFacts, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT sample_json FROM materialised_charge_samples
             WHERE vehicle_id = ?1 AND charge_id = ?2",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_id.to_string(), charge_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(StoreError::Query)?;
    let mut facts = crate::lifecycle::ChargeSampleFacts::default();
    for row in rows {
        let json = row.map_err(StoreError::Query)?;
        let sample = serde_json::from_str::<crate::hub_pack::ProjectionChargeSample>(&json)
            .map_err(StoreError::DeserializeLifecycleRow)?;
        facts.observe(&sample);
    }
    Ok(facts)
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
    sha256_open_file_hex(&mut file)
}

/// The SHA-256 of everything `file` yields from its current position.
fn sha256_open_file_hex(file: &mut fs::File) -> Result<String, StoreError> {
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
