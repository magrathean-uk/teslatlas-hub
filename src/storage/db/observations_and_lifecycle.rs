// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    pub fn put_address_cache(&self, record: &AddressCacheRecord) -> Result<(), StoreError> {
        validate_address_cache_record(record)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        transaction
            .execute(
                "INSERT INTO address_cache(
                    osm_type, osm_id, display_name, name, latitude, longitude,
                    house_number, road, neighbourhood, city, county, postcode,
                    state, state_district, country, raw_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                           ?12, ?13, ?14, ?15, ?16)
                 ON CONFLICT(osm_type, osm_id) DO UPDATE SET
                    display_name = excluded.display_name,
                    name = excluded.name,
                    latitude = excluded.latitude,
                    longitude = excluded.longitude,
                    house_number = excluded.house_number,
                    road = excluded.road,
                    neighbourhood = excluded.neighbourhood,
                    city = excluded.city,
                    county = excluded.county,
                    postcode = excluded.postcode,
                    state = excluded.state,
                    state_district = excluded.state_district,
                    country = excluded.country,
                    raw_json = excluded.raw_json",
                params![
                    record.osm_type,
                    record.osm_id,
                    record.display_name,
                    record.name,
                    record.latitude,
                    record.longitude,
                    record.house_number,
                    record.road,
                    record.neighbourhood,
                    record.city,
                    record.county,
                    record.postcode,
                    record.state,
                    record.state_district,
                    record.country,
                    record.raw_json,
                ],
            )
            .map_err(StoreError::AddressCacheWrite)?;
        transaction
            .execute(
                "INSERT INTO address_lookup_cache(
                    lookup_key, latitude, longitude, osm_type, osm_id, looked_up_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(lookup_key) DO UPDATE SET
                    latitude = excluded.latitude,
                    longitude = excluded.longitude,
                    osm_type = excluded.osm_type,
                    osm_id = excluded.osm_id,
                    looked_up_at_ms = excluded.looked_up_at_ms",
                params![
                    address_lookup_key(crate::location::Wgs84Point {
                        latitude: record.lookup_latitude,
                        longitude: record.lookup_longitude,
                    }),
                    record.lookup_latitude,
                    record.lookup_longitude,
                    record.osm_type,
                    record.osm_id,
                    record.looked_up_at_ms,
                ],
            )
            .map_err(StoreError::AddressCacheWrite)?;
        transaction.commit().map_err(StoreError::AddressCacheWrite)
    }

    pub fn claim_address_enrichment_job(
        &self,
        now_ms: i64,
    ) -> Result<Option<AddressEnrichmentJob>, StoreError> {
        validate_timestamp("address job now_ms", now_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let job = {
            let mut statement = transaction
                .prepare(
                    "SELECT job_key, vehicle_id, target_type, target_id, field,
                            latitude, longitude, attempts
                     FROM address_enrichment_jobs
                     WHERE (status IN ('pending', 'retry') AND next_attempt_ms <= ?1)
                        OR (status = 'running' AND lease_until_ms <= ?1)
                     ORDER BY next_attempt_ms ASC, job_key ASC LIMIT 1",
                )
                .map_err(StoreError::Query)?;
            statement
                .query_row(params![now_ms], |row| {
                    let vehicle_id: String = row.get(1)?;
                    Ok(AddressEnrichmentJob {
                        job_key: row.get(0)?,
                        vehicle_id: Uuid::parse_str(&vehicle_id).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                1,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        target_type: row.get(2)?,
                        target_id: row.get(3)?,
                        field: row.get(4)?,
                        latitude: row.get(5)?,
                        longitude: row.get(6)?,
                        attempts: row
                            .get::<_, i64>(7)?
                            .try_into()
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, i64::MAX))?,
                    })
                })
                .optional()
                .map_err(StoreError::Query)?
        };
        if let Some(job) = &job {
            transaction
                .execute(
                    "UPDATE address_enrichment_jobs
                     SET status = 'running', attempts = attempts + 1,
                         lease_until_ms = ?1
                     WHERE job_key = ?2",
                    params![now_ms.saturating_add(5 * 60 * 1000), job.job_key],
                )
                .map_err(StoreError::AddressEnrichmentWrite)?;
        }
        transaction
            .commit()
            .map_err(StoreError::AddressEnrichmentWrite)?;
        Ok(job.map(|mut job| {
            job.attempts = job.attempts.saturating_add(1);
            job
        }))
    }

    pub fn complete_address_enrichment(
        &self,
        job: &AddressEnrichmentJob,
        address: Option<&str>,
        now_ms: i64,
    ) -> Result<AddressEnrichmentCompletion, StoreError> {
        validate_timestamp("address completion now_ms", now_ms)?;
        if let Some(address) = address
            && (address.trim().is_empty()
                || address.len() > MAX_DISPLAY_NAME_BYTES
                || address.chars().any(char::is_control))
        {
            return Err(StoreError::InvalidAddressEnrichment);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let mut changed = false;
        if let Some(address) = address {
            let (table, json_column, id_column) = match job.target_type.as_str() {
                "drive" => ("materialised_drives", "drive_json", "drive_id"),
                "charge" => ("materialised_charges", "charge_json", "charge_id"),
                _ => return Err(StoreError::InvalidAddressEnrichment),
            };
            let select = format!(
                "SELECT {json_column}, car_id FROM {table} WHERE vehicle_id = ?1 AND {id_column} = ?2"
            );
            let current: Option<(String, i64)> = transaction
                .query_row(
                    &select,
                    params![job.vehicle_id.to_string(), job.target_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(StoreError::Query)?;
            if let Some((current, car_id)) = current {
                let mut value: Value =
                    serde_json::from_str(&current).map_err(StoreError::DeserializeLifecycleRow)?;
                let object = value
                    .as_object_mut()
                    .ok_or(StoreError::InvalidAddressEnrichment)?;
                if object.get(&job.field).and_then(Value::as_str).is_none() {
                    object.insert(job.field.clone(), Value::String(address.trim().to_owned()));
                    let updated =
                        serde_json::to_string(&value).map_err(StoreError::SerializeLifecycleRow)?;
                    let update = format!(
                        "UPDATE {table} SET {json_column} = ?1 WHERE vehicle_id = ?2 AND {id_column} = ?3"
                    );
                    transaction
                        .execute(
                            &update,
                            params![updated, job.vehicle_id.to_string(), job.target_id],
                        )
                        .map_err(StoreError::AddressEnrichmentWrite)?;
                    let entity = if job.target_type == "drive" {
                        "drive"
                    } else {
                        "charge"
                    };
                    record_sync_mutation_in_transaction(
                        &transaction,
                        job.vehicle_id,
                        entity,
                        job.target_id,
                        car_id,
                        "upsert",
                        &updated,
                    )?;
                    changed = true;
                }
            }
        }
        transaction
            .execute(
                "UPDATE address_enrichment_jobs
                 SET status = 'complete', completed_at_ms = ?1, lease_until_ms = 0,
                     last_error = NULL
                 WHERE job_key = ?2",
                params![now_ms, job.job_key],
            )
            .map_err(StoreError::AddressEnrichmentWrite)?;
        if changed {
            mark_export_dirty_in_transaction(&transaction, job.vehicle_id)?;
        }
        transaction
            .commit()
            .map_err(StoreError::AddressEnrichmentWrite)?;
        Ok(AddressEnrichmentCompletion {
            vehicle_id: job.vehicle_id,
            changed,
        })
    }

    pub fn retry_address_enrichment(
        &self,
        job: &AddressEnrichmentJob,
        error: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        validate_timestamp("address retry now_ms", now_ms)?;
        let delay_seconds = 5_u64
            .saturating_mul(1_u64 << job.attempts.min(14))
            .min(24 * 60 * 60);
        let delay_ms = i64::try_from(delay_seconds.saturating_mul(1000)).unwrap_or(i64::MAX);
        let bounded_error = error
            .chars()
            .filter(|character| !character.is_control())
            .take(256)
            .collect::<String>();
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE address_enrichment_jobs
                 SET status = 'retry', next_attempt_ms = ?1, lease_until_ms = 0,
                     last_error = ?2
                 WHERE job_key = ?3",
                params![now_ms.saturating_add(delay_ms), bounded_error, job.job_key],
            )
            .map_err(StoreError::AddressEnrichmentWrite)?;
        Ok(())
    }

    /// Append exactly one bounded raw telemetry snapshot. The stored hash is
    /// calculated from the canonical JSON bytes that are written to SQLite.
    /// A collector retry for the same source, vehicle, observation time, and
    /// payload returns the original row without creating a duplicate.
    pub fn append_observation(
        &self,
        input: &ObservationInput,
        received_at_ms: i64,
    ) -> Result<AppendObservation, StoreError> {
        input.validate()?;
        validate_timestamp("observation received_at_ms", received_at_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let result = append_observation_in_transaction(&transaction, input, received_at_ms);
        if result.is_ok() {
            transaction
                .commit()
                .map_err(StoreError::AppendObservation)?;
        }
        result
    }

    pub(crate) fn accept_stream_observation_and_lifecycle(
        &self,
        input: &ObservationInput,
        received_at_ms: i64,
        car_id: i64,
    ) -> Result<StreamObservationResult, StoreError> {
        let mut connection = self.open()?;
        self.accept_stream_observation_and_lifecycle_on(
            &mut connection,
            input,
            received_at_ms,
            car_id,
        )
    }

    pub(crate) fn stream_observation_writer(&self) -> Result<StreamObservationWriter, StoreError> {
        Ok(StreamObservationWriter {
            store: self.clone(),
            connection: self.open()?,
        })
    }

    pub(crate) fn stream_watermark(&self, vehicle_id: Uuid) -> Result<Option<i64>, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT last_timestamp_ms FROM stream_watermarks WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)
    }

    fn accept_stream_observation_and_lifecycle_on(
        &self,
        connection: &mut Connection,
        input: &ObservationInput,
        received_at_ms: i64,
        car_id: i64,
    ) -> Result<StreamObservationResult, StoreError> {
        input.validate()?;
        validate_timestamp("observation received_at_ms", received_at_ms)?;
        if car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }

        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if !stream_timestamp_is_newer(&transaction, input.vehicle_id, input.observed_at_ms)? {
            transaction.commit().map_err(StoreError::LifecycleWrite)?;
            return Ok(StreamObservationResult::IgnoredDuplicate);
        }
        self.maybe_stream_fault(StreamFaultPoint::RawInsert)?;
        let appended = append_observation_in_transaction(&transaction, input, received_at_ms)?;

        self.maybe_stream_fault(StreamFaultPoint::LifecycleWrite)?;
        let existing = load_lifecycle_state_in_transaction(&transaction, input.vehicle_id)?;
        let mut state = match existing.as_ref() {
            Some(record) => crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                .map_err(|_| StoreError::InvalidLifecycleSession)?,
            None => crate::lifecycle::OpenSessionState::new(),
        };
        // Do not rehydrate full open child collections on every observation.
        // Aggregates in open_session_json plus lifecycle_open_rows are enough
        // for incremental extend; commit reloads children only when a session
        // closes.
        let observations = observations_after_id_in_transaction(
            &transaction,
            input.vehicle_id,
            state.last_observation_id,
            MAX_OBSERVATION_QUERY_LIMIT,
        )?;
        let mut delta = crate::lifecycle::LifecycleDelta::default();
        let mut quarantined = existing.as_ref().is_some_and(|record| record.quarantined);
        for observation in observations {
            let sample = crate::lifecycle::LifecycleSample {
                observation_id: observation.observation_id,
                observed_at_ms: observation.observed_at_ms,
                vehicle_state: observation_vehicle_state(&observation.payload),
                payload: observation.payload,
            };
            let step = crate::lifecycle::apply_sample(state, car_id, &sample)
                .map_err(StoreError::LifecycleProjection)?;
            state = step.state;
            quarantined |= step.quarantined;
            delta.drives.extend(step.delta.drives);
            for discarded_drive_id in &step.delta.discarded_drive_ids {
                delta
                    .open_drive_positions
                    .retain(|position| position.drive_id != Some(*discarded_drive_id));
            }
            delta
                .discarded_drive_ids
                .extend(step.delta.discarded_drive_ids);
            delta.positions.extend(step.delta.positions);
            delta.charges.extend(step.delta.charges);
            delta.charge_samples.extend(step.delta.charge_samples);
            delta.states.extend(step.delta.states);
            delta.updates.extend(step.delta.updates);
            delta
                .charge_start_coordinates
                .extend(step.delta.charge_start_coordinates);
            delta
                .open_drive_positions
                .extend(step.delta.open_drive_positions);
            delta
                .open_charge_samples
                .extend(step.delta.open_charge_samples);
        }
        if let Some(open) = state.open_drive.as_mut() {
            open.positions.clear();
        }
        if let Some(open) = state.open_charge.as_mut() {
            open.samples.clear();
        }
        let encoded = state
            .encode()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        Self::commit_lifecycle_delta_in_transaction(
            &transaction,
            &LifecycleCommit {
                vehicle_id: input.vehicle_id,
                car_id,
                open_session_json: &encoded,
                last_observation_id: state.last_observation_id,
                quarantined,
                updated_at_ms: received_at_ms,
                delta: &delta,
            },
        )?;
        self.maybe_stream_fault(StreamFaultPoint::WatermarkUpdate)?;
        accept_stream_timestamp_in_transaction(
            &transaction,
            input.vehicle_id,
            input.observed_at_ms,
        )?;
        self.maybe_stream_fault(StreamFaultPoint::Commit)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(StreamObservationResult::Committed {
            observation_id: appended.observation.observation_id,
        })
    }

    pub fn accept_owner_observation_and_lifecycle(
        &self,
        input: &ObservationInput,
        received_at_ms: i64,
        car_id: i64,
    ) -> Result<OwnerObservationResult, StoreError> {
        self.accept_owner_observation_and_lifecycle_with_offline_timeout(
            input,
            received_at_ms,
            car_id,
            crate::lifecycle::DEFAULT_OFFLINE_DRIVE_TIMEOUT,
        )
    }

    pub(crate) fn accept_owner_observation_and_lifecycle_with_offline_timeout(
        &self,
        input: &ObservationInput,
        received_at_ms: i64,
        car_id: i64,
        offline_drive_timeout: std::time::Duration,
    ) -> Result<OwnerObservationResult, StoreError> {
        input.validate()?;
        validate_timestamp("observation received_at_ms", received_at_ms)?;
        if car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        self.maybe_stream_fault(StreamFaultPoint::RawInsert)?;
        let appended = append_observation_in_transaction(&transaction, input, received_at_ms)?;
        self.maybe_stream_fault(StreamFaultPoint::LifecycleWrite)?;
        let existing = load_lifecycle_state_in_transaction(&transaction, input.vehicle_id)?;
        let mut state = match existing.as_ref() {
            Some(record) => crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                .map_err(|_| StoreError::InvalidLifecycleSession)?,
            None => crate::lifecycle::OpenSessionState::new(),
        };
        // Incremental path: no full open-child rehydrate per observation.
        let observations = observations_after_id_in_transaction(
            &transaction,
            input.vehicle_id,
            state.last_observation_id,
            MAX_OBSERVATION_QUERY_LIMIT,
        )?;
        let mut delta = crate::lifecycle::LifecycleDelta::default();
        let mut quarantined = existing.as_ref().is_some_and(|record| record.quarantined);
        for observation in observations {
            let sample = crate::lifecycle::LifecycleSample {
                observation_id: observation.observation_id,
                observed_at_ms: observation.observed_at_ms,
                vehicle_state: observation_vehicle_state(&observation.payload),
                payload: observation.payload,
            };
            let step = crate::lifecycle::apply_sample_with_offline_drive_timeout(
                state,
                car_id,
                &sample,
                offline_drive_timeout,
            )
            .map_err(StoreError::LifecycleProjection)?;
            state = step.state;
            quarantined |= step.quarantined;
            delta.drives.extend(step.delta.drives);
            for discarded_drive_id in &step.delta.discarded_drive_ids {
                delta
                    .open_drive_positions
                    .retain(|position| position.drive_id != Some(*discarded_drive_id));
            }
            delta
                .discarded_drive_ids
                .extend(step.delta.discarded_drive_ids);
            delta.positions.extend(step.delta.positions);
            delta.charges.extend(step.delta.charges);
            delta.charge_samples.extend(step.delta.charge_samples);
            delta.states.extend(step.delta.states);
            delta.updates.extend(step.delta.updates);
            delta
                .charge_start_coordinates
                .extend(step.delta.charge_start_coordinates);
            delta
                .open_drive_positions
                .extend(step.delta.open_drive_positions);
            delta
                .open_charge_samples
                .extend(step.delta.open_charge_samples);
        }
        if let Some(open) = state.open_drive.as_mut() {
            open.positions.clear();
        }
        if let Some(open) = state.open_charge.as_mut() {
            open.samples.clear();
        }
        let encoded = state
            .encode()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        Self::commit_lifecycle_delta_in_transaction(
            &transaction,
            &LifecycleCommit {
                vehicle_id: input.vehicle_id,
                car_id,
                open_session_json: &encoded,
                last_observation_id: state.last_observation_id,
                quarantined,
                updated_at_ms: received_at_ms,
                delta: &delta,
            },
        )?;
        self.maybe_stream_fault(StreamFaultPoint::Commit)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(OwnerObservationResult {
            append: appended,
            drives_closed: delta.drives.len(),
            charges_closed: delta.charges.len(),
            positions_materialised: delta.positions.len(),
            charge_samples_materialised: delta.charge_samples.len(),
            lifecycle_quarantined: quarantined,
        })
    }

    /// Advance the durable watermark for stream telemetry. This is deliberately
    /// separate from Owner API observations: each source has its own ordering
    /// contract, and a stream frame must never block an Owner API response.
    pub fn accept_stream_timestamp(
        &self,
        vehicle_id: Uuid,
        timestamp_ms: i64,
    ) -> Result<bool, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        validate_timestamp("stream timestamp", timestamp_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let accepted =
            accept_stream_timestamp_in_transaction(&transaction, vehicle_id, timestamp_ms)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(accepted)
    }

    /// Read a bounded, time-ordered raw observation page for a single stable
    /// Hub vehicle identity.
    pub fn observations_for_vehicle(
        &self,
        vehicle_id: Uuid,
        query: ObservationQuery,
    ) -> Result<Vec<ObservationRecord>, StoreError> {
        query.validate()?;
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms, \
                        payload_sha256, payload_json \
                 FROM raw_observations \
                 WHERE vehicle_id = ?1 \
                   AND (?2 IS NULL OR observed_at_ms >= ?2) \
                   AND (?3 IS NULL OR observed_at_ms < ?3) \
                 ORDER BY observed_at_ms ASC, observation_id ASC \
                 LIMIT ?4",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(
                params![
                    vehicle_id.to_string(),
                    query.from_observed_at_ms,
                    query.until_observed_at_ms,
                    i64::from(query.limit),
                ],
                observation_from_row,
            )
            .map_err(StoreError::Query)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)
    }

    /// Read observations in durable insertion order after a lifecycle cursor.
    /// A lifecycle cursor is an observation ID, not a source timestamp.
    pub fn observations_after_id_for_vehicle(
        &self,
        vehicle_id: Uuid,
        after_observation_id: i64,
        limit: u32,
    ) -> Result<Vec<ObservationRecord>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if after_observation_id < 0 {
            return Err(StoreError::InvalidLifecycleCursor);
        }
        if !(1..=MAX_OBSERVATION_QUERY_LIMIT).contains(&limit) {
            return Err(StoreError::InvalidObservationQueryLimit {
                actual: limit,
                maximum: MAX_OBSERVATION_QUERY_LIMIT,
            });
        }
        let connection = self.open()?;
        let mut statement = connection
            // The schema-56 covering cursor index keeps this incremental for
            // each vehicle. A global rowid scan would repeatedly revisit rows
            // belonging to other vehicles when one vehicle is idle.
            .prepare(OBSERVATIONS_AFTER_ID_SQL)
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(
                params![
                    vehicle_id.to_string(),
                    after_observation_id,
                    i64::from(limit)
                ],
                observation_from_row,
            )
            .map_err(StoreError::Query)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)
    }

    /// Latest durable discovery, Owner API, and streaming observations used by
    /// the bounded current-state endpoint. Processed raw history can be pruned
    /// without losing these three replacement snapshots.
    pub fn current_observations_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Vec<ObservationRecord>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open_read_only_connection()?;
        let exists = connection
            .query_row(
                "SELECT 1 FROM vehicles WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if exists.is_none() {
            return Err(StoreError::UnknownVehicle(vehicle_id));
        }
        let mut statement = connection
            .prepare(
                "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms,
                        payload_sha256, payload_json
                 FROM current_observations WHERE vehicle_id = ?1
                 ORDER BY observed_at_ms, observation_id",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map(params![vehicle_id.to_string()], observation_from_row)
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)
    }

    /// Return only the newest durable current-snapshot metadata. Fleet
    /// observations may be pruned from `raw_observations` after lifecycle
    /// projection, so operator status must read this table without loading the
    /// retained JSON payloads.
    pub fn latest_current_observation_metadata_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<LatestObservationMetadata>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open_read_only_connection()?;
        let exists = connection
            .query_row(
                "SELECT 1 FROM vehicles WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if exists.is_none() {
            return Err(StoreError::UnknownVehicle(vehicle_id));
        }
        connection
            .query_row(
                "SELECT observation_id, observed_at_ms, received_at_ms
                 FROM current_observations
                 WHERE vehicle_id = ?1
                 ORDER BY observation_id DESC LIMIT 1",
                params![vehicle_id.to_string()],
                |row| {
                    Ok(LatestObservationMetadata {
                        observation_id: row.get(0)?,
                        observed_at_ms: row.get(1)?,
                        received_at_ms: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::Query)
    }

    pub fn materialised_car_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<crate::hub_pack::ProjectionCar>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Query)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(StoreError::DeserializeLifecycleRow)
    }

    pub fn materialised_drive_for_vehicle(
        &self,
        vehicle_id: Uuid,
        drive_id: i64,
    ) -> Result<Option<crate::hub_pack::ProjectionDrive>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if drive_id <= 0 {
            return Err(StoreError::InvalidDriveId);
        }
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT drive_json FROM materialised_drives
                 WHERE vehicle_id = ?1 AND drive_id = ?2",
                params![vehicle_id.to_string(), drive_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Query)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(StoreError::DeserializeLifecycleRow)
    }

    /// One bounded page of a drive's positions in TeslaMate GPX order.
    pub fn drive_positions_page(
        &self,
        vehicle_id: Uuid,
        drive_id: i64,
        after: Option<(i64, i64)>,
        limit: u32,
    ) -> Result<Vec<crate::hub_pack::ProjectionPosition>, StoreError> {
        const MAXIMUM_PAGE: u32 = 10_000;
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if drive_id <= 0 {
            return Err(StoreError::InvalidDriveId);
        }
        if limit == 0 || limit > MAXIMUM_PAGE {
            return Err(StoreError::InvalidDrivePositionPageLimit(limit));
        }
        let (after_date_ms, after_id) = after.unwrap_or((-1, -1));
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT position_json FROM materialised_positions
                 WHERE vehicle_id = ?1 AND drive_id = ?2
                   AND (CAST(json_extract(position_json, '$.date_ms') AS INTEGER) > ?3
                        OR (CAST(json_extract(position_json, '$.date_ms') AS INTEGER) = ?3
                            AND position_id > ?4))
                 ORDER BY CAST(json_extract(position_json, '$.date_ms') AS INTEGER), position_id
                 LIMIT ?5",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map(
                params![
                    vehicle_id.to_string(),
                    drive_id,
                    after_date_ms,
                    after_id,
                    i64::from(limit)
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(StoreError::Query)?
            .map(|value| {
                value.map_err(StoreError::Query).and_then(|value| {
                    serde_json::from_str(&value).map_err(StoreError::DeserializeLifecycleRow)
                })
            })
            .collect()
    }

    pub fn set_charge_cost(
        &self,
        vehicle_id: Uuid,
        charge_id: i64,
        cost: f64,
    ) -> Result<crate::hub_pack::ProjectionCharge, StoreError> {
        self.set_charge_cost_input(vehicle_id, charge_id, cost, None)
    }

    pub fn set_charge_cost_rate(
        &self,
        vehicle_id: Uuid,
        charge_id: i64,
        rate: f64,
        billing_type: crate::hub_pack::GeofenceBillingType,
    ) -> Result<crate::hub_pack::ProjectionCharge, StoreError> {
        self.set_charge_cost_input(vehicle_id, charge_id, rate, Some(billing_type))
    }

    fn set_charge_cost_input(
        &self,
        vehicle_id: Uuid,
        charge_id: i64,
        value: f64,
        billing_type: Option<crate::hub_pack::GeofenceBillingType>,
    ) -> Result<crate::hub_pack::ProjectionCharge, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if charge_id <= 0 {
            return Err(StoreError::InvalidChargeId);
        }
        if !value.is_finite() || !(0.0..=1_000_000_000.0).contains(&value) {
            return Err(StoreError::InvalidChargeCost);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let charge_json = transaction
            .query_row(
                "SELECT charge_json FROM materialised_charges
                 WHERE vehicle_id = ?1 AND charge_id = ?2",
                params![vehicle_id.to_string(), charge_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Query)?
            .ok_or(StoreError::UnknownCharge(charge_id))?;
        let mut charge: crate::hub_pack::ProjectionCharge =
            serde_json::from_str(&charge_json).map_err(StoreError::DeserializeLifecycleRow)?;
        let cost = match billing_type {
            None => value,
            Some(billing_type) => crate::lifecycle::calculate_charge_cost(
                charge.fast_charger_type.as_deref(),
                false,
                charge.charge_energy_added,
                charge.charge_energy_used_kwh,
                charge.duration_min,
                Some(crate::lifecycle::ChargeTariff {
                    billing_type,
                    cost_per_unit: Some(value),
                    session_fee: None,
                }),
            )
            .ok_or(StoreError::ChargeCostBasisUnavailable {
                charge_id,
                mode: billing_type.as_str(),
            })?,
        };
        if charge.cost == Some(cost) {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(charge);
        }
        charge.cost = Some(cost);
        let payload = serde_json::to_string(&charge).map_err(StoreError::SerializeLifecycleRow)?;
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
        mark_export_dirty_in_transaction(&transaction, vehicle_id)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(charge)
    }

    pub fn geofence_name_at(
        &self,
        vehicle_id: Uuid,
        latitude: f64,
        longitude: f64,
    ) -> Result<Option<String>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let fences = load_geofence_fences(&connection, vehicle_id)?;
        Ok(crate::lifecycle::match_geofence_name(
            latitude, longitude, &fences,
        ))
    }

    /// Load durable open-session state for crash-safe lifecycle recovery.
    pub fn load_lifecycle_state(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<LifecycleStateRecord>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT vehicle_id, car_id, last_observation_id, open_session_json, \
                        quarantined, updated_at_ms \
                 FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| {
                    let value: String = row.get(0)?;
                    let vehicle_id = Uuid::parse_str(&value).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
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

    /// Rehydrate provisional drive/charge children without placing them in
    /// the bounded lifecycle JSON document.
    /// Durable open drive positions for one parent (incremental lifecycle path).
    pub fn open_drive_positions(
        &self,
        vehicle_id: Uuid,
        drive_id: i64,
    ) -> Result<Vec<crate::hub_pack::ProjectionPosition>, StoreError> {
        let connection = self.open()?;
        let vehicle = vehicle_id.to_string();
        let mut statement = connection
            .prepare(
                "SELECT row_json FROM lifecycle_open_rows
                 WHERE vehicle_id = ?1 AND domain = 'position'
                   AND parent_source_row_id = ?2
                 ORDER BY source_row_id",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![vehicle, drive_id], |row| row.get::<_, String>(0))
            .map_err(StoreError::Query)?;
        let mut positions = Vec::new();
        for row in rows {
            let json = row.map_err(StoreError::Query)?;
            positions
                .push(serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?);
        }
        Ok(positions)
    }

    /// Durable open charge samples for one parent (incremental lifecycle path).
    pub fn open_charge_samples(
        &self,
        vehicle_id: Uuid,
        charge_id: i64,
    ) -> Result<Vec<crate::hub_pack::ProjectionChargeSample>, StoreError> {
        let connection = self.open()?;
        let vehicle = vehicle_id.to_string();
        let mut statement = connection
            .prepare(
                "SELECT row_json FROM lifecycle_open_rows
                 WHERE vehicle_id = ?1 AND domain = 'charge_sample'
                   AND parent_source_row_id = ?2
                 ORDER BY source_row_id",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![vehicle, charge_id], |row| row.get::<_, String>(0))
            .map_err(StoreError::Query)?;
        let mut samples = Vec::new();
        for row in rows {
            let json = row.map_err(StoreError::Query)?;
            samples.push(serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?);
        }
        Ok(samples)
    }

    pub fn restore_lifecycle_open_children(
        &self,
        vehicle_id: Uuid,
        state: &mut crate::lifecycle::OpenSessionState,
    ) -> Result<(), StoreError> {
        let connection = self.open()?;
        let vehicle = vehicle_id.to_string();
        let mut statement = connection
            .prepare(
                "SELECT domain, parent_source_row_id, row_json
                 FROM lifecycle_open_rows WHERE vehicle_id = ?1
                 ORDER BY source_row_id",
            )
            .map_err(StoreError::Query)?;
        if let Some(open) = state.open_drive.as_mut() {
            open.positions.clear();
        }
        if let Some(open) = state.open_charge.as_mut() {
            open.samples.clear();
        }
        let rows = statement
            .query_map(params![vehicle], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(StoreError::Query)?;
        for row in rows {
            let (domain, parent_id, json) = row.map_err(StoreError::Query)?;
            match domain.as_str() {
                "position" => {
                    let position: crate::hub_pack::ProjectionPosition =
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?;
                    if state
                        .open_drive
                        .as_ref()
                        .is_some_and(|open| Some(open.id) == parent_id)
                    {
                        state
                            .open_drive
                            .as_mut()
                            .expect("open drive")
                            .positions
                            .push(position);
                    }
                }
                "charge_sample" => {
                    let sample: crate::hub_pack::ProjectionChargeSample =
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?;
                    if state
                        .open_charge
                        .as_ref()
                        .is_some_and(|open| Some(open.id) == parent_id)
                    {
                        state
                            .open_charge
                            .as_mut()
                            .expect("open charge")
                            .samples
                            .push(sample);
                    }
                }
                _ => {}
            }
        }
        if let Some(open) = state.open_drive.as_mut() {
            if let Some(first) = open.positions.first() {
                open.start_latitude = Some(first.latitude);
                open.start_longitude = Some(first.longitude);
                open.start_soc = first.battery_level;
                open.start_rated_range_km = first.rated_battery_range_km;
            }
            open.outside_temp_sum = 0.0;
            open.outside_temp_count = 0;
            open.speed_max = None;
            for position in &open.positions {
                if let Some(value) = position.outside_temp {
                    open.outside_temp_sum += value;
                    open.outside_temp_count = open.outside_temp_count.saturating_add(1);
                }
                open.speed_max = match (open.speed_max, position.speed) {
                    (Some(current), Some(next)) => Some(current.max(next)),
                    (None, value) => value,
                    (current, None) => current,
                };
            }
        }
        Ok(())
    }

    /// Atomically retain an imported open-session snapshot outside the bounded
    /// lifecycle blob. Repeating the same source snapshot is a no-op.
    pub fn seed_imported_open_session(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        session: &TeslaMateOpenSession,
        updated_at_ms: i64,
    ) -> Result<OpenSessionSeedReport, StoreError> {
        self.seed_imported_open_session_checked(
            source_id,
            vehicle_id,
            car_id,
            session,
            updated_at_ms,
            None,
        )
    }

    fn seed_imported_open_session_checked(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        session: &TeslaMateOpenSession,
        updated_at_ms: i64,
        expected: Option<(i64, i64)>,
    ) -> Result<OpenSessionSeedReport, StoreError> {
        if source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }
        validate_timestamp("open session updated_at_ms", updated_at_ms)?;
        session
            .validate()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;

        let previous = self.load_lifecycle_state(vehicle_id)?;
        let previous_state = previous
            .as_ref()
            .map(|record| {
                crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                    .map_err(|_| StoreError::InvalidLifecycleSession)
            })
            .transpose()?;
        let previous_open = self.load_imported_open_session(source_id, vehicle_id)?;
        let seeded = crate::lifecycle::seed_imported_open_session_state(
            source_id,
            session,
            previous_state.as_ref(),
        )
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
        let same_seed = previous_state
            .as_ref()
            .and_then(|state| state.imported_open.as_ref())
            .is_some_and(|refs| {
                refs.source_id == source_id.to_string()
                    && refs.drive_source_row_id == session.drive.as_ref().map(|row| row.id)
                    && refs.charge_source_row_id == session.charge.as_ref().map(|row| row.id)
                    && refs.state_source_row_id == session.state.as_ref().map(|row| row.id)
                    && refs.standalone_position_count == session.standalone_positions.len() as u64
            })
            && previous_open.as_ref().is_some_and(|old| old == session);
        if same_seed {
            return Ok(OpenSessionSeedReport {
                no_op: true,
                ..OpenSessionSeedReport::default()
            });
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if let Some((expected_last_observation_id, expected_updated_at_ms)) = expected {
            let actual: Option<(i64, i64)> = transaction
                .query_row(
                    "SELECT last_observation_id, updated_at_ms
                     FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                    params![vehicle_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(StoreError::LifecycleWrite)?;
            if actual != Some((expected_last_observation_id, expected_updated_at_ms)) {
                return Err(StoreError::ImportGenerationConflict);
            }
        }
        ensure_source_exists(&transaction, source_id)?;
        ensure_vehicle_source(&transaction, vehicle_id, source_id)?;

        let source = source_id.to_string();
        let vehicle = vehicle_id.to_string();
        transaction
            .execute(
                "DELETE FROM lifecycle_open_rows
                 WHERE source_id = ?1 AND vehicle_id = ?2",
                params![source, vehicle],
            )
            .map_err(StoreError::LifecycleWrite)?;
        transaction
            .execute(
                "DELETE FROM lifecycle_source_watermarks
                 WHERE source_id = ?1 AND vehicle_id = ?2",
                params![source, vehicle],
            )
            .map_err(StoreError::LifecycleWrite)?;
        let mut inserted = 0;
        if let Some(row) = &session.drive {
            inserted += insert_open_row(
                &transaction,
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
                &transaction,
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
                &transaction,
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
                &transaction,
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
                &transaction,
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
                &transaction,
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
            let json =
                serde_json::to_string(&position).map_err(StoreError::SerializeLifecycleRow)?;
            standalone_positions_inserted += transaction
                .execute(
                    "INSERT INTO materialised_positions(
                        vehicle_id, position_id, drive_id, car_id, position_json,
                        speed, power, est_battery_range_km, fan_status,
                        driver_temp_setting, passenger_temp_setting,
                        is_climate_on, is_rear_defroster_on, is_front_defroster_on,
                        battery_heater, battery_heater_on, battery_heater_no_power,
                        tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr
                     ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                               ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
                     ON CONFLICT(vehicle_id, position_id) DO NOTHING",
                    params![
                        vehicle,
                        position.id,
                        car_id,
                        json,
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
            transaction
                .execute(
                    "INSERT INTO lifecycle_source_watermarks(
                        source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(source_id, vehicle_id, domain) DO UPDATE SET
                        max_source_row_id = MAX(max_source_row_id, excluded.max_source_row_id),
                        max_timestamp_ms = MAX(max_timestamp_ms, excluded.max_timestamp_ms)",
                    params![
                        source,
                        vehicle,
                        domain,
                        watermark.max_id,
                        watermark.max_timestamp_ms
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        let json = seeded
            .encode()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        transaction
            .execute(
                "INSERT INTO vehicle_lifecycle_state(
                    vehicle_id, car_id, last_observation_id, open_session_json,
                    quarantined, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, 0, ?5)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    car_id = excluded.car_id,
                    open_session_json = excluded.open_session_json,
                    updated_at_ms = MAX(updated_at_ms, excluded.updated_at_ms)",
                params![
                    vehicle,
                    car_id,
                    previous
                        .as_ref()
                        .map_or(0, |record| record.last_observation_id),
                    json,
                    updated_at_ms,
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        mark_export_dirty_in_transaction(&transaction, vehicle_id)?;

        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(OpenSessionSeedReport {
            provisional_rows_inserted: inserted,
            standalone_positions_inserted,
            watermarks_written: watermarks.len(),
            no_op: false,
        })
    }

    /// Reconstruct the full imported open-session view after a restart.
    pub fn load_imported_open_session(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
    ) -> Result<Option<TeslaMateOpenSession>, StoreError> {
        if source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT domain, row_json FROM lifecycle_open_rows
                 WHERE source_id = ?1 AND vehicle_id = ?2
                 ORDER BY source_table, source_row_id",
            )
            .map_err(StoreError::Query)?;
        let mut session = TeslaMateOpenSession::default();
        let mut found = false;
        let rows = statement
            .query_map(
                params![source_id.to_string(), vehicle_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(StoreError::Query)?;
        for row in rows {
            let (domain, json) = row.map_err(StoreError::Query)?;
            found = true;
            match domain.as_str() {
                "drive" => {
                    session.drive = Some(
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                    )
                }
                "position" => session.drive_positions.push(
                    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                "charge" => {
                    session.charge = Some(
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                    )
                }
                "charge_sample" => session.charge_samples.push(
                    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                "state" => {
                    session.state = Some(
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                    )
                }
                "standalone_position" => session.standalone_positions.push(
                    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                _ => return Err(StoreError::InvalidLifecycleSession),
            }
        }
        let mut watermark_statement = connection
            .prepare(
                "SELECT domain, max_source_row_id, max_timestamp_ms
                 FROM lifecycle_source_watermarks
                 WHERE source_id = ?1 AND vehicle_id = ?2",
            )
            .map_err(StoreError::Query)?;
        let watermarks = watermark_statement
            .query_map(
                params![source_id.to_string(), vehicle_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        crate::teslamate_projection::TeslaMateSourceWatermark {
                            max_id: row.get(1)?,
                            max_timestamp_ms: row.get(2)?,
                        },
                    ))
                },
            )
            .map_err(StoreError::Query)?;
        for watermark in watermarks {
            let (domain, value) = watermark.map_err(StoreError::Query)?;
            match domain.as_str() {
                "drives" => session.watermarks.drives = value,
                "positions" => session.watermarks.positions = value,
                "charging_processes" => session.watermarks.charging_processes = value,
                "charges" => session.watermarks.charges = value,
                "states" => session.watermarks.states = value,
                "updates" => session.watermarks.updates = value,
                _ => return Err(StoreError::InvalidLifecycleSession),
            }
        }
        if !found {
            return Ok(None);
        }
        session.car_id = session
            .drive
            .as_ref()
            .map(|row| row.car_id)
            .or_else(|| session.charge.as_ref().map(|row| row.car_id))
            .or_else(|| session.state.as_ref().map(|row| row.car_id))
            .or_else(|| session.drive_positions.first().map(|row| row.car_id))
            .unwrap_or_default();
        session
            .validate()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        Ok(Some(session))
    }
}
