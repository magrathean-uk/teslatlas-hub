// SPDX-License-Identifier: AGPL-3.0-only

fn append_observation_in_transaction(
    transaction: &Transaction<'_>,
    input: &ObservationInput,
    received_at_ms: i64,
) -> Result<AppendObservation, StoreError> {
    input.validate()?;
    validate_timestamp("observation received_at_ms", received_at_ms)?;
    let payload_json =
        serde_json::to_vec(&input.payload).map_err(StoreError::SerializeObservation)?;
    if payload_json.len() > MAX_RAW_OBSERVATION_BYTES {
        return Err(StoreError::ObservationTooLarge {
            actual: payload_json.len(),
            maximum: MAX_RAW_OBSERVATION_BYTES,
        });
    }
    let payload_sha256 = Sha256Digest::of_bytes(&payload_json);
    let payload_json = String::from_utf8(payload_json).expect("serde_json is UTF-8");
    ensure_vehicle_belongs_to_source(transaction, input.vehicle_id, input.source_id)?;
    let record_type = input
        .payload
        .get("record_type")
        .and_then(Value::as_str)
        .filter(|value| {
            matches!(
                *value,
                "owner_api_discovery_v1"
                    | "owner_api_vehicle_data_v1"
                    | "fleet_api_discovery_v1"
                    | "fleet_api_vehicle_data_v1"
                    | "tesla_stream_update_v1"
            )
        });
    if let Some(record_type) = record_type
        && let Some(current) =
            current_observation_for_type(transaction, input.vehicle_id, record_type)?
        && (current.observed_at_ms > input.observed_at_ms
            || (current.observed_at_ms == input.observed_at_ms
                && current.payload_sha256 == payload_sha256))
    {
        return Ok(AppendObservation {
            observation: current,
            inserted: false,
        });
    }
    let inserted = transaction
        .execute(
            "INSERT INTO raw_observations
             (observation_id, source_id, vehicle_id, observed_at_ms,
              received_at_ms, payload_sha256, payload_json)
             VALUES (
                 COALESCE((
                     SELECT MAX(observation_id) FROM (
                         SELECT observation_id FROM raw_observations
                         UNION ALL
                         SELECT observation_id FROM current_observations
                     )
                 ), 0) + 1,
                 ?1, ?2, ?3, ?4, ?5, ?6
             )
             ON CONFLICT(source_id, vehicle_id, observed_at_ms, payload_sha256) DO NOTHING",
            params![
                input.source_id.to_string(),
                input.vehicle_id.to_string(),
                input.observed_at_ms,
                received_at_ms,
                payload_sha256.as_bytes().as_slice(),
                payload_json,
            ],
        )
        .map_err(StoreError::AppendObservation)?
        == 1;
    let observation = find_observation(
        transaction,
        input.source_id,
        input.vehicle_id,
        input.observed_at_ms,
        payload_sha256,
    )?
    .ok_or(StoreError::ObservationMissingAfterInsert)?;
    if let Some(record_type) = record_type {
        transaction
            .execute(
                "INSERT INTO current_observations(
                    vehicle_id, record_type, observation_id, source_id,
                    observed_at_ms, received_at_ms, payload_sha256, payload_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(vehicle_id, record_type) DO UPDATE SET
                    observation_id = excluded.observation_id,
                    source_id = excluded.source_id,
                    observed_at_ms = excluded.observed_at_ms,
                    received_at_ms = excluded.received_at_ms,
                    payload_sha256 = excluded.payload_sha256,
                    payload_json = excluded.payload_json
                 WHERE excluded.observed_at_ms > current_observations.observed_at_ms
                    OR (excluded.observed_at_ms = current_observations.observed_at_ms
                        AND excluded.observation_id > current_observations.observation_id)",
                params![
                    observation.vehicle_id.to_string(),
                    record_type,
                    observation.observation_id,
                    observation.source_id.to_string(),
                    observation.observed_at_ms,
                    observation.received_at_ms,
                    observation.payload_sha256.as_bytes().as_slice(),
                    payload_json,
                ],
            )
            .map_err(StoreError::AppendObservation)?;
    }
    Ok(AppendObservation {
        observation,
        inserted,
    })
}

fn current_observation_for_type(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    record_type: &str,
) -> Result<Option<ObservationRecord>, StoreError> {
    transaction
        .query_row(
            "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms,
                    payload_sha256, payload_json
             FROM current_observations
             WHERE vehicle_id = ?1 AND record_type = ?2",
            params![vehicle_id.to_string(), record_type],
            observation_from_row,
        )
        .optional()
        .map_err(StoreError::Query)
}

fn prune_processed_observations(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    through_observation_id: i64,
) -> Result<(), StoreError> {
    if through_observation_id <= 0 {
        return Ok(());
    }
    transaction
        .execute(
            "INSERT INTO raw_observation_prune_guard(singleton) VALUES(1)",
            [],
        )
        .map_err(StoreError::LifecycleWrite)?;
    transaction
        .execute(
            "DELETE FROM raw_observations
             WHERE vehicle_id = ?1 AND observation_id <= ?2",
            params![vehicle_id.to_string(), through_observation_id],
        )
        .map_err(StoreError::LifecycleWrite)?;
    transaction
        .execute("DELETE FROM raw_observation_prune_guard", [])
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn stream_timestamp_is_newer(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    timestamp_ms: i64,
) -> Result<bool, StoreError> {
    let previous: Option<i64> = transaction
        .query_row(
            "SELECT last_timestamp_ms FROM stream_watermarks WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    Ok(previous.is_none_or(|value| timestamp_ms > value))
}

fn accept_stream_timestamp_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    timestamp_ms: i64,
) -> Result<bool, StoreError> {
    validate_timestamp("stream timestamp", timestamp_ms)?;
    Ok(transaction
        .execute(
            "INSERT INTO stream_watermarks(vehicle_id, last_timestamp_ms)
             VALUES (?1, ?2)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                 last_timestamp_ms = excluded.last_timestamp_ms
             WHERE excluded.last_timestamp_ms > stream_watermarks.last_timestamp_ms",
            params![vehicle_id.to_string(), timestamp_ms],
        )
        .map_err(StoreError::Query)?
        == 1)
}

fn load_lifecycle_state_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<Option<LifecycleStateRecord>, StoreError> {
    transaction
        .query_row(
            "SELECT vehicle_id, car_id, last_observation_id, open_session_json,
                    quarantined, updated_at_ms
             FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| {
                let vehicle_id = row
                    .get::<_, String>(0)?
                    .parse()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                Ok(LifecycleStateRecord {
                    vehicle_id,
                    car_id: row.get(1)?,
                    last_observation_id: row.get(2)?,
                    open_session_json: row.get(3)?,
                    quarantined: row.get::<_, i64>(4)? != 0,
                    updated_at_ms: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::Query)
}

fn observations_after_id_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    after_observation_id: i64,
    limit: u32,
) -> Result<Vec<ObservationRecord>, StoreError> {
    if after_observation_id < 0 {
        return Err(StoreError::InvalidLifecycleCursor);
    }
    if !(1..=MAX_OBSERVATION_QUERY_LIMIT).contains(&limit) {
        return Err(StoreError::InvalidObservationQueryLimit {
            actual: limit,
            maximum: MAX_OBSERVATION_QUERY_LIMIT,
        });
    }
    let mut statement = transaction
        .prepare(OBSERVATIONS_AFTER_ID_SQL)
        .map_err(StoreError::Query)?;
    statement
        .query_map(
            params![
                vehicle_id.to_string(),
                after_observation_id,
                i64::from(limit)
            ],
            observation_from_row,
        )
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)
}

fn load_open_positions_for_parent(
    transaction: &Transaction<'_>,
    vehicle_key: &str,
    drive_id: i64,
) -> Result<Vec<crate::hub_pack::ProjectionPosition>, StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT row_json FROM lifecycle_open_rows
             WHERE vehicle_id = ?1 AND domain = 'position'
               AND parent_source_row_id = ?2
             ORDER BY source_row_id",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_key, drive_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(StoreError::Query)?;
    let mut positions = Vec::new();
    for row in rows {
        let json = row.map_err(StoreError::Query)?;
        positions.push(serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?);
    }
    Ok(positions)
}

fn load_open_charge_samples_for_parent(
    transaction: &Transaction<'_>,
    vehicle_key: &str,
    charge_id: i64,
) -> Result<Vec<crate::hub_pack::ProjectionChargeSample>, StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT row_json FROM lifecycle_open_rows
             WHERE vehicle_id = ?1 AND domain = 'charge_sample'
               AND parent_source_row_id = ?2
             ORDER BY source_row_id",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_key, charge_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(StoreError::Query)?;
    let mut samples = Vec::new();
    for row in rows {
        let json = row.map_err(StoreError::Query)?;
        samples.push(serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?);
    }
    Ok(samples)
}

// Full open-child rehydrate was removed from the hot observation path.
// Close materialization loads children once via load_open_*_for_parent.

fn observation_vehicle_state(payload: &Value) -> String {
    payload
        .get("source_vehicle_state")
        .and_then(Value::as_str)
        .filter(|state| {
            !state.is_empty() && state.len() <= 64 && !state.chars().any(char::is_control)
        })
        .unwrap_or("unknown")
        .to_owned()
}

fn ensure_vehicle_source(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    source_id: Uuid,
) -> Result<(), StoreError> {
    let actual: Option<i64> = transaction
        .query_row(
            "SELECT 1 FROM vehicle_identity_aliases
             WHERE vehicle_id = ?1 AND source_id = ?2",
            params![vehicle_id.to_string(), source_id.to_string()],
            |_| Ok(1),
        )
        .optional()
        .map_err(StoreError::LifecycleWrite)?;
    let Some(actual) = actual else {
        return Err(StoreError::UnknownVehicle(vehicle_id));
    };
    let _ = actual;
    Ok(())
}

fn insert_open_row<T: Serialize>(
    transaction: &Transaction<'_>,
    source_id: &str,
    source_table: &str,
    source_row_id: i64,
    vehicle_id: &str,
    car_id: i64,
    domain: &str,
    parent_source_row_id: Option<i64>,
    row: &T,
) -> Result<usize, StoreError> {
    let row_json = serde_json::to_string(row).map_err(StoreError::SerializeLifecycleRow)?;
    transaction
        .execute(
            "INSERT INTO lifecycle_open_rows(
                source_id, source_table, source_row_id, vehicle_id, car_id,
                domain, parent_source_row_id, row_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(source_id, vehicle_id, source_table, source_row_id) DO NOTHING",
            params![
                source_id,
                source_table,
                source_row_id,
                vehicle_id,
                car_id,
                domain,
                parent_source_row_id,
                row_json,
            ],
        )
        .map_err(StoreError::LifecycleWrite)
}

fn mark_export_dirty_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO export_outbox(
                vehicle_id, dirty_revision, attempts, next_attempt_ms,
                claimed_until_ms, last_error
             ) VALUES (?1, 1, 0, 0, 0, NULL)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                dirty_revision = export_outbox.dirty_revision + 1,
                -- Keep an active lease fenced to its current publisher. The
                -- terminal transition will release the newer revision without
                -- deleting it; an expired lease remains immediately claimable.
                attempts = CASE WHEN export_outbox.claimed_until_ms > 0
                    THEN export_outbox.attempts ELSE 0 END,
                next_attempt_ms = CASE WHEN export_outbox.claimed_until_ms > 0
                    THEN export_outbox.next_attempt_ms ELSE 0 END,
                last_error = NULL",
            params![vehicle_id.to_string()],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}
