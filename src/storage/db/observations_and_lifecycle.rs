// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    /// Store one geocoder lookup under its own version-2 key.
    ///
    /// The lookup row keeps the complete record this response returned
    /// (display name, name, address parts, raw JSON, licence) beside the
    /// binding it was asked under, and a later lookup only ever rewrites a row
    /// with the very same key. The shared `address_cache` row for the OSM
    /// identity is written only when it is absent and is never updated: it is
    /// the identity the foreign key points at, not something any lookup reads.
    pub fn put_address_cache(&self, record: &AddressCacheRecord) -> Result<(), StoreError> {
        validate_address_cache_record(record)?;
        let key = address_lookup_key(
            &record.binding,
            crate::location::Wgs84Point {
                latitude: record.lookup_latitude,
                longitude: record.lookup_longitude,
            },
        );
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
                 ON CONFLICT(osm_type, osm_id) DO NOTHING",
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
                    lookup_key, latitude, longitude, osm_type, osm_id, looked_up_at_ms,
                    provider, endpoint, endpoint_host, language, licence,
                    display_name, name, result_latitude, result_longitude,
                    house_number, road, neighbourhood, city, county, postcode,
                    state, state_district, country, raw_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                           ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22,
                           ?23, ?24, ?25)
                 ON CONFLICT(lookup_key) DO UPDATE SET
                    latitude = excluded.latitude,
                    longitude = excluded.longitude,
                    osm_type = excluded.osm_type,
                    osm_id = excluded.osm_id,
                    looked_up_at_ms = excluded.looked_up_at_ms,
                    provider = excluded.provider,
                    endpoint = excluded.endpoint,
                    endpoint_host = excluded.endpoint_host,
                    language = excluded.language,
                    licence = excluded.licence,
                    display_name = excluded.display_name,
                    name = excluded.name,
                    result_latitude = excluded.result_latitude,
                    result_longitude = excluded.result_longitude,
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
                    key,
                    record.lookup_latitude,
                    record.lookup_longitude,
                    record.osm_type,
                    record.osm_id,
                    record.looked_up_at_ms,
                    record.binding.provider,
                    record.binding.endpoint,
                    record.binding.endpoint_host,
                    record.binding.language,
                    record.licence,
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

    /// Last accepted reachability of a vehicle, `online`, `asleep` or
    /// `offline`, from its persisted lifecycle state (C1 R7, R77). `None` when
    /// the vehicle has no state or no reachability has been accepted yet. The
    /// collector reads it inside its tick transaction
    /// (`collector_lifecycle_snapshot_in_transaction`, C3 R14); this standalone
    /// read is for tests.
    #[cfg(test)]
    pub(crate) fn lifecycle_last_reach(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<String>, StoreError> {
        let Some(record) = self.load_lifecycle_state(vehicle_id)? else {
            return Ok(None);
        };
        crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
            .map(|state| state.last_reach)
            .map_err(|_| StoreError::InvalidLifecycleSession)
    }

    /// Record a successful configured-vehicle discovery as current presence.
    ///
    /// A discovery whose state equals the vehicle's persisted `last_reach` is
    /// freshness evidence only: it goes to `current_observations` and never
    /// reaches the lifecycle. A discovery that differs is delivered to the
    /// lifecycle (stored in `raw_observations` and projected) in the same
    /// transaction, so a reachability change the unit has not yet seen is never
    /// lost, for example asleep, a wake seen only through the stream or
    /// `vehicle_data`, then asleep again (C1 R77, section 7 item 8). Only the
    /// three Tesla reachability texts are delivered; any other state text gives
    /// the unit no evidence and would otherwise be stored on every poll.
    /// Returns whether the discovery was delivered. The collector's tick
    /// transaction applies C3 R14's rule instead (`discovery_action`), which also
    /// withholds an unchanged state's write; this function is kept for tests of
    /// the intake's ordering rules.
    #[cfg(test)]
    pub(crate) fn record_current_discovery(
        &self,
        input: &ObservationInput,
        received_at_ms: i64,
        offline_drive_timeout: std::time::Duration,
    ) -> Result<bool, StoreError> {
        input.validate()?;
        validate_timestamp("discovery received_at_ms", received_at_ms)?;
        Self::validate_current_discovery(input)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        ensure_vehicle_belongs_to_source(&transaction, input.vehicle_id, input.source_id)?;
        let discovered = observation_vehicle_state(&input.payload);
        if matches!(discovered.as_str(), "online" | "asleep" | "offline")
            && let Some(record) =
                load_lifecycle_state_in_transaction(&transaction, input.vehicle_id)?
        {
            let state = crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                .map_err(|_| StoreError::InvalidLifecycleSession)?;
            if state.last_reach.as_deref() != Some(discovered.as_str()) && record.car_id > 0 {
                self.accept_owner_observation_and_lifecycle_in_transaction(
                    &transaction,
                    input,
                    received_at_ms,
                    record.car_id,
                    offline_drive_timeout,
                )?;
                transaction
                    .commit()
                    .map_err(StoreError::AppendObservation)?;
                return Ok(true);
            }
        }
        Self::write_current_discovery_in_transaction(&transaction, input, received_at_ms)?;
        transaction
            .commit()
            .map_err(StoreError::AppendObservation)?;
        Ok(false)
    }

    /// A discovery payload must carry one of the two discovery record types and
    /// fit the raw-observation bound.
    fn validate_current_discovery(input: &ObservationInput) -> Result<(), StoreError> {
        let record_type = input.payload.get("record_type").and_then(Value::as_str);
        if !matches!(
            record_type,
            Some("owner_api_discovery_v1" | "fleet_api_discovery_v1")
        ) {
            return Err(StoreError::InvalidCurrentDiscovery);
        }
        let size = serde_json::to_vec(&input.payload)
            .map_err(StoreError::SerializeObservation)?
            .len();
        if size > MAX_RAW_OBSERVATION_BYTES {
            return Err(StoreError::ObservationTooLarge {
                actual: size,
                maximum: MAX_RAW_OBSERVATION_BYTES,
            });
        }
        Ok(())
    }

    /// Write only the vehicle's current discovery row, inside the caller's
    /// transaction, which owns the commit (C3 R14 "missing current row": a
    /// discovery that equals the lifecycle's last reachability stores no raw row,
    /// but a vehicle without a current discovery row still gets that row). The
    /// row is replaced only by a newer or different discovery.
    pub(crate) fn write_current_discovery_in_transaction(
        transaction: &Transaction<'_>,
        input: &ObservationInput,
        received_at_ms: i64,
    ) -> Result<(), StoreError> {
        input.validate()?;
        validate_timestamp("discovery received_at_ms", received_at_ms)?;
        Self::validate_current_discovery(input)?;
        let record_type = input
            .payload
            .get("record_type")
            .and_then(Value::as_str)
            .ok_or(StoreError::InvalidCurrentDiscovery)?;
        let payload =
            serde_json::to_vec(&input.payload).map_err(StoreError::SerializeObservation)?;
        let digest = Sha256Digest::of_bytes(&payload);
        let payload = String::from_utf8(payload).expect("serde_json is UTF-8");
        ensure_vehicle_belongs_to_source(transaction, input.vehicle_id, input.source_id)?;
        transaction
            .execute(
                concat!(
                    "INSERT INTO current_observations(
                vehicle_id,record_type,observation_id,source_id,
                observed_at_ms,received_at_ms,payload_sha256,payload_json
             ) VALUES (
                ?1,?2,
                ",
                    next_observation_id_sql!(),
                    ",
                ?3,?4,?5,?6,?7
             ) ON CONFLICT(vehicle_id,record_type) DO UPDATE SET
                observation_id=excluded.observation_id,
                source_id=excluded.source_id,
                observed_at_ms=excluded.observed_at_ms,
                received_at_ms=excluded.received_at_ms,
                payload_sha256=excluded.payload_sha256,
                payload_json=excluded.payload_json
             WHERE excluded.observed_at_ms > current_observations.observed_at_ms
                OR (excluded.observed_at_ms = current_observations.observed_at_ms
                    AND excluded.payload_sha256 != current_observations.payload_sha256)"
                ),
                params![
                    input.vehicle_id.to_string(),
                    record_type,
                    input.source_id.to_string(),
                    input.observed_at_ms,
                    received_at_ms,
                    digest.as_bytes().as_slice(),
                    payload,
                ],
            )
            .map_err(StoreError::AppendObservation)?;
        Ok(())
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
            crate::lifecycle::merge_step_delta(&mut delta, step.delta);
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
        let result = self.accept_owner_observation_and_lifecycle_in_transaction(
            &transaction,
            input,
            received_at_ms,
            car_id,
            offline_drive_timeout,
        )?;
        self.maybe_stream_fault(StreamFaultPoint::Commit)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(result)
    }

    /// Join the caller's transaction without opening or committing another one.
    /// The caller must roll back on error and owns the final commit/fault boundary.
    pub(crate) fn accept_owner_observation_and_lifecycle_in_transaction(
        &self,
        transaction: &rusqlite::Transaction<'_>,
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
        self.maybe_stream_fault(StreamFaultPoint::RawInsert)?;
        let appended = append_observation_in_transaction(transaction, input, received_at_ms)?;
        self.maybe_stream_fault(StreamFaultPoint::LifecycleWrite)?;
        let existing = load_lifecycle_state_in_transaction(transaction, input.vehicle_id)?;
        let mut state = match existing.as_ref() {
            Some(record) => crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                .map_err(|_| StoreError::InvalidLifecycleSession)?,
            None => crate::lifecycle::OpenSessionState::new(),
        };
        // Incremental path: no full open-child rehydrate per observation.
        let observations = observations_after_id_in_transaction(
            transaction,
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
            crate::lifecycle::merge_step_delta(&mut delta, step.delta);
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
        let (positions_materialised, charge_samples_materialised) =
            Self::commit_lifecycle_delta_in_transaction(
                transaction,
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
        Ok(OwnerObservationResult {
            append: appended,
            drives_closed: delta.drives.len(),
            charges_closed: delta.charges.len(),
            positions_materialised,
            charge_samples_materialised,
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

    /// One bounded newest-first page of completed drives in a half-open UTC
    /// millisecond range. The extra row lets the HTTP layer issue a cursor
    /// without a second count query. The caller has already checked that the
    /// vehicle is active and published (`vehicle_is_published_and_active`).
    pub fn materialised_drives_page(
        &self,
        vehicle_id: Uuid,
        from_ms: i64,
        to_ms: i64,
        before: Option<(i64, i64)>,
        limit: u32,
    ) -> Result<Vec<crate::hub_pack::ProjectionDrive>, StoreError> {
        const MAXIMUM_FETCH: u32 = 501;
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if from_ms < 0 || to_ms < 0 || from_ms >= to_ms {
            return Err(StoreError::InvalidDriveQueryRange);
        }
        if limit == 0 || limit > MAXIMUM_FETCH {
            return Err(StoreError::InvalidDriveQueryLimit(limit));
        }
        if before.is_some_and(|(before_start_ms, before_drive_id)| {
            before_start_ms < from_ms || before_start_ms >= to_ms || before_drive_id <= 0
        }) {
            return Err(StoreError::InvalidDriveQueryCursor);
        }
        let (before_start_ms, before_drive_id) = before.unwrap_or((i64::MAX, i64::MAX));
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(PUBLIC_DRIVES_PAGE_SQL)
            .map_err(StoreError::Query)?;
        statement
            .query_map(
                params![
                    vehicle_id.to_string(),
                    from_ms,
                    to_ms,
                    before_start_ms,
                    before_drive_id,
                    i64::from(limit)
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(StoreError::Query)?
            .map(|value| {
                let (stored_drive_id, value) = value.map_err(StoreError::Query)?;
                let drive: crate::hub_pack::ProjectionDrive =
                    serde_json::from_str(&value).map_err(StoreError::DeserializeLifecycleRow)?;
                if drive.id != stored_drive_id {
                    return Err(StoreError::InvalidDriveProjectionIdentity);
                }
                Ok(drive)
            })
            .collect()
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

    /// One closed drive of one vehicle for the public read API. A drive
    /// without an end date is not served and reads as absent. Unlike
    /// `materialised_drive_for_vehicle` this checks the JSON identity.
    pub fn materialised_closed_drive_for_vehicle(
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
                PUBLIC_DRIVE_ITEM_SQL,
                params![vehicle_id.to_string(), drive_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?
            .map(|(stored_drive_id, value)| {
                let drive: crate::hub_pack::ProjectionDrive =
                    serde_json::from_str(&value).map_err(StoreError::DeserializeLifecycleRow)?;
                if drive.id != stored_drive_id {
                    return Err(StoreError::InvalidDriveProjectionIdentity);
                }
                Ok(drive)
            })
            .transpose()
    }

    /// One bounded page of a drive's positions for the public read API, in
    /// `(date_ms, id)` order inside the half-open window `[from_ms, to_ms)`,
    /// after the keyset `after`. The caller looks the parent drive up first.
    /// Additive: `drive_positions_page` and the GPX export are unchanged.
    pub fn drive_positions_window_page(
        &self,
        vehicle_id: Uuid,
        drive_id: i64,
        from_ms: i64,
        to_ms: i64,
        after: Option<(i64, i64)>,
        limit: u32,
    ) -> Result<Vec<PublicPositionRow>, StoreError> {
        const MAXIMUM_FETCH: u32 = 2049;
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if drive_id <= 0 {
            return Err(StoreError::InvalidDriveId);
        }
        let (after_date_ms, after_id) =
            validate_public_page_arguments(from_ms, to_ms, after, limit, MAXIMUM_FETCH, (-1, 0))?;
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(PUBLIC_POSITIONS_PAGE_SQL)
            .map_err(StoreError::Query)?;
        statement
            .query_map(
                params![
                    vehicle_id.to_string(),
                    drive_id,
                    from_ms,
                    to_ms,
                    after_date_ms,
                    after_id,
                    i64::from(limit)
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .map_err(StoreError::Query)?
            .map(|row| {
                let (stored_position_id, stored_drive_id, value, status, dataset, version, tile) =
                    row.map_err(StoreError::Query)?;
                let position: crate::hub_pack::ProjectionPosition =
                    serde_json::from_str(&value).map_err(StoreError::DeserializeLifecycleRow)?;
                if position.id != stored_position_id
                    || stored_drive_id != drive_id
                    || position.drive_id != Some(drive_id)
                {
                    return Err(StoreError::InvalidPublicProjectionIdentity("position"));
                }
                Ok(PublicPositionRow {
                    position,
                    terrain: (status.as_deref() == Some("success")).then_some(
                        PositionTerrainLookup {
                            dataset,
                            dataset_version: version,
                            tile,
                        },
                    ),
                })
            })
            .collect()
    }

    /// One closed charge session of one vehicle for the public read API. A
    /// session without an end date is not served and reads as absent.
    pub fn materialised_charge_for_vehicle(
        &self,
        vehicle_id: Uuid,
        charge_id: i64,
    ) -> Result<Option<crate::hub_pack::ProjectionCharge>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if charge_id <= 0 {
            return Err(StoreError::InvalidChargeId);
        }
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                PUBLIC_CHARGE_ITEM_SQL,
                params![vehicle_id.to_string(), charge_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?
            .map(|(stored_charge_id, value)| {
                let charge: crate::hub_pack::ProjectionCharge =
                    serde_json::from_str(&value).map_err(StoreError::DeserializeLifecycleRow)?;
                if charge.id != stored_charge_id {
                    return Err(StoreError::InvalidPublicProjectionIdentity("charge"));
                }
                Ok(charge)
            })
            .transpose()
    }

    /// One bounded newest-first page of closed charge sessions inside the
    /// half-open window `[from_ms, to_ms)` on the start date. The extra row
    /// lets the HTTP layer issue a cursor without a second count query. The
    /// caller has already checked that the vehicle is active and published.
    pub fn materialised_charges_page(
        &self,
        vehicle_id: Uuid,
        from_ms: i64,
        to_ms: i64,
        before: Option<(i64, i64)>,
        limit: u32,
    ) -> Result<Vec<crate::hub_pack::ProjectionCharge>, StoreError> {
        const MAXIMUM_FETCH: u32 = 501;
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let (before_start_ms, before_charge_id) = validate_public_page_arguments(
            from_ms,
            to_ms,
            before,
            limit,
            MAXIMUM_FETCH,
            (i64::MAX, i64::MAX),
        )?;
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(PUBLIC_CHARGES_PAGE_SQL)
            .map_err(StoreError::Query)?;
        statement
            .query_map(
                params![
                    vehicle_id.to_string(),
                    from_ms,
                    to_ms,
                    before_start_ms,
                    before_charge_id,
                    i64::from(limit)
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(StoreError::Query)?
            .map(|row| {
                let (stored_charge_id, value) = row.map_err(StoreError::Query)?;
                let charge: crate::hub_pack::ProjectionCharge =
                    serde_json::from_str(&value).map_err(StoreError::DeserializeLifecycleRow)?;
                if charge.id != stored_charge_id {
                    return Err(StoreError::InvalidPublicProjectionIdentity("charge"));
                }
                Ok(charge)
            })
            .collect()
    }

    /// One bounded page of a charge's samples for the public read API, in
    /// `(timestamp_ms, id)` order inside the half-open window `[from_ms, to_ms)`,
    /// after the keyset `after`. The caller looks the parent charge up first.
    pub fn charge_samples_page(
        &self,
        vehicle_id: Uuid,
        charge_id: i64,
        from_ms: i64,
        to_ms: i64,
        after: Option<(i64, i64)>,
        limit: u32,
    ) -> Result<Vec<crate::hub_pack::ProjectionChargeSample>, StoreError> {
        const MAXIMUM_FETCH: u32 = 2049;
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if charge_id <= 0 {
            return Err(StoreError::InvalidChargeId);
        }
        let (after_timestamp_ms, after_id) =
            validate_public_page_arguments(from_ms, to_ms, after, limit, MAXIMUM_FETCH, (-1, 0))?;
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(PUBLIC_CHARGE_SAMPLES_PAGE_SQL)
            .map_err(StoreError::Query)?;
        statement
            .query_map(
                params![
                    vehicle_id.to_string(),
                    charge_id,
                    from_ms,
                    to_ms,
                    after_timestamp_ms,
                    after_id,
                    i64::from(limit)
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(StoreError::Query)?
            .map(|row| {
                let (stored_sample_id, stored_charge_id, value) =
                    row.map_err(StoreError::Query)?;
                let sample: crate::hub_pack::ProjectionChargeSample =
                    serde_json::from_str(&value).map_err(StoreError::DeserializeLifecycleRow)?;
                if sample.id != stored_sample_id
                    || stored_charge_id != charge_id
                    || sample.charge_process_id != charge_id
                {
                    return Err(StoreError::InvalidPublicProjectionIdentity("charge sample"));
                }
                Ok(sample)
            })
            .collect()
    }

    /// Set the cost of one materialised charge to the total the owner gives
    /// (C2 R53 `total`): `cost = round2(cost)`, the tariff copy unchanged. A
    /// total may be negative (a credit); it must fit the pack's scale-2 fixed
    /// numeric (R55 (c)). Allowed on every materialised row, imported ones
    /// included, and it replaces any earlier cost. Returns the stored row.
    pub fn set_charge_cost(
        &self,
        vehicle_id: Uuid,
        charge_id: i64,
        cost: f64,
    ) -> Result<crate::hub_pack::ProjectionCharge, StoreError> {
        self.set_charge_cost_input(vehicle_id, charge_id, cost, None)
    }

    /// Set the cost of one materialised charge from a unit price the owner
    /// gives (C2 R53 `per_kwh` and `per_minute`). The tariff copy becomes
    /// `(billing_type, round4(rate), no fee)` and the cost is the R44 cost on
    /// that copy with no free charging and no fence fee, because the owner's
    /// entry is the complete price. Returns the stored row.
    pub fn set_charge_cost_rate(
        &self,
        vehicle_id: Uuid,
        charge_id: i64,
        rate: f64,
        billing_type: crate::hub_pack::GeofenceBillingType,
    ) -> Result<crate::hub_pack::ProjectionCharge, StoreError> {
        self.set_charge_cost_input(vehicle_id, charge_id, rate, Some(billing_type))
    }

    /// The shared path of the two manual setters (C2 R53 to R55), in the order
    /// R54 gives: a nil vehicle id, a charge id below one, a value that is not
    /// finite or is outside its range, a charge that is not materialised, a
    /// rate with no basis to price, a computed cost outside the pack's range.
    /// On any error nothing is written. On success one `IMMEDIATE`
    /// transaction updates the row, adds one `charge` sync mutation and marks
    /// the export dirty; entering the value the row already holds writes
    /// nothing and still returns the row (R6).
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
        // R55 (b) and (c), decided before any storage is touched.
        let entered = match billing_type {
            Some(_) => {
                let rate = round_manual_amount(value, MANUAL_RATE_DECIMALS);
                if !rate.is_some_and(|rate| (0.0..=MANUAL_RATE_MAX).contains(&rate)) {
                    return Err(StoreError::InvalidChargeCost);
                }
                rate
            }
            None => {
                let total = round_manual_amount(value, MANUAL_TOTAL_DECIMALS);
                if !total.is_some_and(crate::lifecycle::cost_fits_pack_scale) {
                    return Err(StoreError::InvalidChargeCost);
                }
                total
            }
        }
        .ok_or(StoreError::InvalidChargeCost)?;

        let vehicle_key = vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let stored: Option<String> = transaction
            .query_row(
                "SELECT charge_json FROM materialised_charges
                 WHERE vehicle_id = ?1 AND charge_id = ?2",
                params![vehicle_key, charge_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let stored: crate::hub_pack::ProjectionCharge = serde_json::from_str(
            &stored.ok_or(StoreError::UnknownCharge(charge_id))?,
        )
        .map_err(StoreError::DeserializeLifecycleRow)?;
        let mut row = stored.clone();
        match billing_type {
            None => row.cost = Some(entered),
            Some(billing_type) => {
                let tariff = crate::lifecycle::ChargeTariff {
                    billing_type,
                    cost_per_unit: Some(entered),
                    session_fee: None,
                };
                // Only a per-minute cost reads the span, so only then are the
                // stored samples loaded (R43a).
                let span_observed = match billing_type {
                    crate::hub_pack::GeofenceBillingType::PerMinute => {
                        crate::lifecycle::charge_span_observed_from(
                            &row,
                            &fold_materialised_charge_samples(&transaction, vehicle_id, charge_id)?,
                        )
                    }
                    crate::hub_pack::GeofenceBillingType::PerKwh => true,
                };
                let context = crate::lifecycle::ChargeCostContext {
                    free_supercharging: false,
                    span_observed,
                };
                let Some(cost) =
                    crate::lifecycle::calculate_charge_cost(&row, Some(tariff), context)
                else {
                    // No cost has two reasons: nothing to price (R42, R43), or
                    // a price out of the pack's range (R55 (c)). A free rate
                    // over the same row tells them apart: it is priced
                    // whenever there is a basis, whatever the rate.
                    let free = crate::lifecycle::ChargeTariff {
                        cost_per_unit: Some(0.0),
                        ..tariff
                    };
                    return Err(
                        if crate::lifecycle::calculate_charge_cost(&row, Some(free), context)
                            .is_none()
                        {
                            StoreError::ChargeCostBasisUnavailable {
                                charge_id,
                                mode: billing_type.as_str(),
                            }
                        } else {
                            StoreError::InvalidChargeCost
                        },
                    );
                };
                row.billing_type = Some(billing_type);
                row.cost_per_unit = Some(entered);
                row.session_fee = None;
                row.cost = Some(cost);
            }
        }
        let changed = row != stored;
        if changed {
            let charge_json =
                serde_json::to_string(&row).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_charges SET charge_json = ?3
                     WHERE vehicle_id = ?1 AND charge_id = ?2",
                    params![vehicle_key, charge_id, charge_json],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                &transaction,
                vehicle_id,
                "charge",
                charge_id,
                row.car_id,
                "upsert",
                &charge_json,
            )?;
            mark_export_dirty_in_transaction(&transaction, vehicle_id)?;
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        // R59: the vehicle id, the charge id and the outcome only (R7).
        tracing::info!(
            vehicle_id = %vehicle_id,
            charge_id,
            changed,
            "manual charge cost stored"
        );
        Ok(row)
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
        let connection = self.open_read_only_connection()?;
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
        let connection = self.open_read_only_connection()?;
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

    /// The vehicle's import watermarks as id floors (C1 R84): per id space the
    /// largest `max_source_row_id` of any source, 0 when there is none.
    pub(crate) fn lifecycle_watermark_id_floors(
        &self,
        vehicle_id: Uuid,
    ) -> Result<crate::lifecycle::IdFloors, StoreError> {
        let connection = self.open()?;
        watermark_id_floors(&connection, vehicle_id)
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
        let mut previous_open = self.load_imported_open_session(source_id, vehicle_id)?;
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
            && previous_open
                .as_mut()
                .is_some_and(|old| imported_open_session_is_unchanged(old, session));
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
        // The `charging_processes` watermark is kept: the upsert below only
        // raises it, because a reseed must never lower the vehicle's import
        // bound W(V) (C2 R5), which would turn imported charge rows into Hub
        // rows that a fill or the efficiency median may then use.
        transaction
            .execute(
                "DELETE FROM lifecycle_source_watermarks
                 WHERE source_id = ?1 AND vehicle_id = ?2 AND domain <> ?3",
                params![source, vehicle, CHARGING_PROCESSES_DOMAIN],
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
                        max_source_row_id = COALESCE(
                            MAX(max_source_row_id, excluded.max_source_row_id),
                            max_source_row_id, excluded.max_source_row_id),
                        max_timestamp_ms = COALESCE(
                            MAX(max_timestamp_ms, excluded.max_timestamp_ms),
                            max_timestamp_ms, excluded.max_timestamp_ms)",
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
        let mut seeded = seeded;
        raise_id_cursors_above_watermarks(&transaction, vehicle_id, &mut seeded)?;
        seed_imported_version_from_updates(&transaction, vehicle_id, &mut seeded)?;
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
            .or_else(|| session.standalone_positions.first().map(|row| row.car_id))
            .unwrap_or_default();
        session
            .validate()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        Ok(Some(session))
    }
}

/// A drive position read for the public API with its recorded terrain lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct PublicPositionRow {
    pub position: crate::hub_pack::ProjectionPosition,
    /// The terrain lookup that succeeded for this position, if any.
    pub terrain: Option<PositionTerrainLookup>,
}

/// What `terrain_elevation_provenance` records about a successful lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionTerrainLookup {
    pub dataset: Option<String>,
    pub dataset_version: Option<String>,
    pub tile: Option<String>,
}

/// Shared argument validation of the public page queries. Returns the keyset
/// to bind: the cursor's last keys, or `unbounded` for a first page.
fn validate_public_page_arguments(
    from_ms: i64,
    to_ms: i64,
    cursor: Option<(i64, i64)>,
    limit: u32,
    maximum_fetch: u32,
    unbounded: (i64, i64),
) -> Result<(i64, i64), StoreError> {
    if from_ms < 0 || to_ms < 0 || from_ms >= to_ms {
        return Err(StoreError::InvalidPublicPageQuery("time range"));
    }
    if limit == 0 || limit > maximum_fetch {
        return Err(StoreError::InvalidPublicPageQuery("limit"));
    }
    if cursor.is_some_and(|(key_a, key_b)| key_a < from_ms || key_a >= to_ms || key_b <= 0) {
        return Err(StoreError::InvalidPublicPageQuery("cursor"));
    }
    Ok(cursor.unwrap_or(unbounded))
}

/// Decimals of a manual unit price (C2 R55 (b)): the scale of a Hub-saved
/// fence price (C8 section 1.1), so one rate policy holds on a fence and on a
/// charge.
const MANUAL_RATE_DECIMALS: i32 = 4;

/// Largest manual unit price (C2 R55 (b)), the upper bound of a Hub-saved fence
/// price (C8 R8): a typing slip of a whole order of magnitude is refused
/// rather than stored as a price.
const MANUAL_RATE_MAX: f64 = 99_999.999_9;

/// Decimals of a manual total (C2 R55 (c)): the scale of the cost column
/// (R4), so the stored value does not change on a pack round trip.
const MANUAL_TOTAL_DECIMALS: i32 = 2;

/// Round `value` half away from zero to `decimals` places, with a result of
/// `-0.0` written `0.0` (C2 R55 (b) and (c)). `None` when the value or the
/// scaled value is not finite, so an overflowing entry is refused instead of
/// being stored as infinity.
fn round_manual_amount(value: f64, decimals: i32) -> Option<f64> {
    if !value.is_finite() {
        return None;
    }
    let scale = 10_f64.powi(decimals);
    let scaled = value * scale;
    if !scaled.is_finite() {
        return None;
    }
    let rounded = scaled.round() / scale;
    Some(if rounded == 0.0 { 0.0 } else { rounded })
}

/// The larger of two known watermark values, the known one when only one is
/// known, `None` when neither is (C2 R73): the value the raise-only
/// `charging_processes` upsert stores.
fn raise_only_value(stored: Option<i64>, snapshot: Option<i64>) -> Option<i64> {
    match (stored, snapshot) {
        (Some(stored), Some(snapshot)) => Some(stored.max(snapshot)),
        (stored, snapshot) => stored.or(snapshot),
    }
}

/// Whether the stored imported open session already equals the snapshot as the
/// seed would leave it (C2 R73). The seed replaces every watermark domain but
/// `charging_processes` by the snapshot's and stores the raise-only merge of
/// the stored and the snapshot's `charging_processes` watermark (R5: no rewrite
/// lowers the vehicle's import bound). The stored session is therefore
/// unchanged when its open rows and its other watermarks equal the snapshot's
/// and its `charging_processes` watermark already equals that merge, so a
/// snapshot whose charge maximum or charge time is absent or not above the
/// stored value is a no-op when repeated. `stored` is left with the snapshot's
/// own `charging_processes` watermark, which the caller no longer uses.
fn imported_open_session_is_unchanged(
    stored: &mut crate::teslamate_projection::TeslaMateOpenSession,
    snapshot: &crate::teslamate_projection::TeslaMateOpenSession,
) -> bool {
    let kept = stored.watermarks.charging_processes;
    let incoming = snapshot.watermarks.charging_processes;
    let merged = crate::teslamate_projection::TeslaMateSourceWatermark {
        max_id: raise_only_value(kept.max_id, incoming.max_id),
        max_timestamp_ms: raise_only_value(kept.max_timestamp_ms, incoming.max_timestamp_ms),
    };
    if kept != merged {
        return false;
    }
    stored.watermarks.charging_processes = incoming;
    *stored == *snapshot
}
