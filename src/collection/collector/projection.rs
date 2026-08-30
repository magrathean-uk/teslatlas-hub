// SPDX-License-Identifier: AGPL-3.0-only

#[cfg(test)]
async fn persist_discovery_events(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
) -> Result<(), CollectorError> {
    persist_discovery_events_with_timeout(
        store,
        cursor_key,
        vehicles,
        CollectorProvider::Legacy,
        crate::lifecycle::DEFAULT_OFFLINE_DRIVE_TIMEOUT,
    )
    .await
}

async fn persist_discovery_events_with_timeout(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    provider: CollectorProvider,
    offline_drive_timeout: Duration,
) -> Result<(), CollectorError> {
    let publication_gate = store.acquire_publication_gate().await?;
    let observed_at_ms = current_epoch_millis()?;
    let source = store.register_source(&provider_source(provider), observed_at_ms)?;
    for vehicle in vehicles {
        let mut descriptor = VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string())
            .with_tesla_identity(Some(vehicle.id.get() as i64), None);
        descriptor.vin = clean_optional_text(Some(&vehicle.vin));
        descriptor.display_name = clean_optional_text(vehicle.display_name.as_deref());
        let registered = store.register_vehicle(&descriptor, observed_at_ms)?;
        let pack_car_id =
            projection_car_id_for_vehicle(store, registered.vehicle_id, vehicle.id.get())?;
        let mut live_settings = vehicle.settings.clone();
        live_settings.suspend_min_resolved = false;
        store.upsert_car_settings(registered.vehicle_id, pack_car_id, &live_settings)?;
        store.accept_owner_observation_and_lifecycle_with_offline_timeout(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: registered.vehicle_id,
                observed_at_ms,
                payload: discovery_payload(vehicle, provider),
            },
            observed_at_ms,
            pack_car_id,
            offline_drive_timeout,
        )?;
    }
    let collection = ManualCollection {
        vehicles: vehicles.to_vec(),
        snapshots: Vec::new(),
        failures: Vec::new(),
    };
    publish_compatibility_snapshots_for_provider(
        store,
        &publication_gate,
        cursor_key,
        &collection,
        observed_at_ms,
        provider,
    )?;
    Ok(())
}

fn discovery_payload(vehicle: &Vehicle, provider: CollectorProvider) -> Value {
    serde_json::json!({
        "record_type": provider_discovery_record_type(provider),
        "source_vehicle_id": vehicle.id.get().to_string(),
        "source_vehicle_state": vehicle.state,
    })
}

fn poll_phase(snapshot: &VehicleData) -> PollPhase {
    let fields = snapshot.fields();
    let updating = fields
        .get("vehicle_state")
        .and_then(Value::as_object)
        .and_then(|vehicle| vehicle.get("software_update"))
        .and_then(Value::as_object)
        .and_then(|update| update.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("installing"));
    if updating {
        return PollPhase::Updating;
    }
    let charging = fields
        .get("charge_state")
        .and_then(Value::as_object)
        .and_then(|charge| charge.get("charging_state"))
        .and_then(Value::as_str)
        .is_some_and(|state| {
            matches!(state.to_ascii_lowercase().as_str(), "starting" | "charging")
        });
    if charging {
        return PollPhase::Charging;
    }
    let drive = fields.get("drive_state").and_then(Value::as_object);
    let shift = drive
        .and_then(|drive| drive.get("shift_state"))
        .and_then(Value::as_str);
    if matches!(shift, Some("D" | "R" | "N" | "d" | "r" | "n")) {
        PollPhase::Driving
    } else {
        PollPhase::Online
    }
}

#[cfg(test)]
fn sleep_eligible(snapshot: &VehicleData) -> bool {
    sleep_eligible_with_policy(snapshot, true)
}

fn sleep_eligible_with_policy(snapshot: &VehicleData, req_not_unlocked: bool) -> bool {
    let fields = snapshot.fields();
    let Some(drive) = fields.get("drive_state").and_then(Value::as_object) else {
        return false;
    };
    let Some(climate) = fields.get("climate_state").and_then(Value::as_object) else {
        return false;
    };
    let Some(vehicle) = fields.get("vehicle_state").and_then(Value::as_object) else {
        return false;
    };
    if poll_phase(snapshot) != PollPhase::Online {
        return false;
    }
    let true_field = |fields: &Map<String, Value>, name: &str| {
        fields.get(name).and_then(Value::as_bool) == Some(true)
    };
    if true_field(vehicle, "is_user_present")
        || true_field(vehicle, "sentry_mode")
        || (req_not_unlocked && vehicle.get("locked").and_then(Value::as_bool) != Some(true))
        || true_field(climate, "is_preconditioning")
        || climate
            .get("climate_keeper_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("dog"))
        || drive.get("power").and_then(Value::as_i64).unwrap_or(0) > 0
    {
        return false;
    }
    for field in ["df", "pf", "dr", "pr", "ft", "rt"] {
        if vehicle.get(field).and_then(Value::as_i64).unwrap_or(0) > 0 {
            return false;
        }
    }
    if vehicle
        .get("software_update")
        .and_then(Value::as_object)
        .is_some_and(|update| {
            update
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status.eq_ignore_ascii_case("downloading"))
                && update
                    .get("download_perc")
                    .and_then(Value::as_f64)
                    .is_none_or(|percent| percent < 100.0)
        })
    {
        return false;
    }
    true
}

fn snapshot_service_mode(snapshot: &VehicleData) -> Option<bool> {
    snapshot
        .fields()
        .get("vehicle_state")
        .and_then(Value::as_object)
        .and_then(|state| state.get("service_mode"))
        .and_then(Value::as_bool)
}

/// Persist one completed compatibility collection. The supplied receipt time
/// makes storage tests deterministic; production obtains it from the system
/// clock only after the HTTP read succeeds.
pub fn persist_collection(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
) -> Result<ManualCollectionReport, CollectorError> {
    persist_collection_mode(
        store,
        collection,
        received_at_ms,
        false,
        CollectorProvider::Legacy,
    )
}

fn persist_collection_atomic_for_provider(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
    provider: CollectorProvider,
) -> Result<ManualCollectionReport, CollectorError> {
    persist_collection_mode(store, collection, received_at_ms, true, provider)
}

fn persist_collection_mode(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
    atomic_lifecycle: bool,
    provider: CollectorProvider,
) -> Result<ManualCollectionReport, CollectorError> {
    if received_at_ms < 0 {
        return Err(CollectorError::InvalidReceiptTimestamp);
    }

    let source = store.register_source(&provider_source(provider), received_at_ms)?;
    let mut vehicles = std::collections::BTreeMap::new();
    let mut online_vehicles_seen = 0;

    for vehicle in &collection.vehicles {
        if vehicle.is_online() {
            online_vehicles_seen += 1;
        }
        let mut descriptor = VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string())
            .with_tesla_identity(Some(vehicle.id.get() as i64), None);
        descriptor.vin = Some(vehicle.vin.clone());
        descriptor.display_name = vehicle.display_name.clone();
        let registered = store.register_vehicle(&descriptor, received_at_ms)?;
        vehicles.insert(vehicle.id.get(), registered.vehicle_id);
    }

    let mut observations_inserted = 0;
    let mut observations_already_present = 0;
    let mut lifecycle_report = LifecycleMaterialisationReport::default();
    let vehicle_states = collection
        .vehicles
        .iter()
        .map(|vehicle| (vehicle.id.get(), vehicle.state.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    for snapshot in &collection.snapshots {
        let source_vehicle_id = snapshot.vehicle_id().get();
        let vehicle_id = vehicles
            .get(&source_vehicle_id)
            .copied()
            .ok_or(CollectorError::SnapshotWithoutListedVehicle)?;
        let source_vehicle_state = vehicle_states
            .get(&source_vehicle_id)
            .copied()
            .ok_or(CollectorError::SnapshotWithoutListedVehicle)?;
        let input = ObservationInput {
            source_id: source.source_id,
            vehicle_id,
            observed_at_ms: observation_timestamp(snapshot, received_at_ms),
            payload: observation_payload(snapshot, source_vehicle_state, provider),
        };
        let append = if atomic_lifecycle {
            let pack_car_id = projection_car_id_for_vehicle(store, vehicle_id, source_vehicle_id)?;
            let result = store.accept_owner_observation_and_lifecycle(
                &input,
                received_at_ms,
                pack_car_id,
            )?;
            lifecycle_report.drives_closed += result.drives_closed;
            lifecycle_report.charges_closed += result.charges_closed;
            lifecycle_report.positions_materialised += result.positions_materialised;
            lifecycle_report.charge_samples_materialised += result.charge_samples_materialised;
            lifecycle_report.lifecycle_quarantines += usize::from(result.lifecycle_quarantined);
            result.append
        } else {
            store.append_observation(&input, received_at_ms)?
        };
        if append.inserted {
            observations_inserted += 1;
        } else {
            observations_already_present += 1;
        }
    }

    Ok(ManualCollectionReport {
        source_id: source.source_id,
        request_audit_correlation_id: Uuid::nil(),
        vehicles_seen: collection.vehicles.len(),
        online_vehicles_seen,
        snapshots_received: collection.snapshots.len(),
        observations_inserted,
        observations_already_present,
        snapshots_published: 0,
        vehicle_failures: collection.failures.len(),
        drives_closed: lifecycle_report.drives_closed,
        charges_closed: lifecycle_report.charges_closed,
        positions_materialised: lifecycle_report.positions_materialised,
        charge_samples_materialised: lifecycle_report.charge_samples_materialised,
        lifecycle_quarantines: lifecycle_report.lifecycle_quarantines,
    })
}

#[derive(Debug, Default)]
pub struct LifecycleMaterialisationReport {
    pub drives_closed: usize,
    pub charges_closed: usize,
    pub positions_materialised: usize,
    pub charge_samples_materialised: usize,
    pub lifecycle_quarantines: usize,
}

/// Project newly stored observations into durable drive/charge history and
/// crash-safe open-session state. Pure projection lives in `lifecycle`; this
/// function only loads the cursor, applies samples, and commits the delta.
pub fn materialise_lifecycle_for_collection(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
) -> Result<LifecycleMaterialisationReport, CollectorError> {
    materialise_lifecycle_for_collection_provider(
        store,
        collection,
        received_at_ms,
        CollectorProvider::Legacy,
    )
}

fn materialise_lifecycle_for_collection_provider(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
    provider: CollectorProvider,
) -> Result<LifecycleMaterialisationReport, CollectorError> {
    let source = store.register_source(&provider_source(provider), received_at_ms)?;
    let mut report = LifecycleMaterialisationReport::default();
    for vehicle in &collection.vehicles {
        let mut descriptor = VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string())
            .with_tesla_identity(Some(vehicle.id.get() as i64), None);
        descriptor.vin = clean_optional_text(Some(&vehicle.vin));
        descriptor.display_name = clean_optional_text(vehicle.display_name.as_deref());
        let registered = store.register_vehicle(&descriptor, received_at_ms)?;
        let car_id = projection_car_id_for_vehicle(store, registered.vehicle_id, vehicle.id.get())?;
        let mut live_settings = vehicle.settings.clone();
        live_settings.suspend_min_resolved = false;
        store.upsert_car_settings(registered.vehicle_id, car_id, &live_settings)?;
        let latest_snapshot = collection
            .snapshots
            .iter()
            .find(|snapshot| snapshot.vehicle_id() == vehicle.id);
        let seed_car = compatibility_car(vehicle, latest_snapshot, car_id);
        store.persist_materialised_car_if_absent(registered.vehicle_id, &seed_car)?;
        let materialised =
            materialise_vehicle_lifecycle(store, registered.vehicle_id, car_id, received_at_ms)?;
        let state = store
            .load_lifecycle_state(registered.vehicle_id)?
            .and_then(|record| OpenSessionState::decode(&record.open_session_json).ok());
        if let Some(metadata) = state.and_then(|state| state.car_metadata) {
            store.resolve_car_suspend_min(
                registered.vehicle_id,
                metadata.model.as_deref(),
                metadata.trim_badging.as_deref(),
                metadata.marketing_name.as_deref(),
            )?;
        }
        report.drives_closed += materialised.drives_closed;
        report.charges_closed += materialised.charges_closed;
        report.positions_materialised += materialised.positions_materialised;
        report.charge_samples_materialised += materialised.charge_samples_materialised;
        report.lifecycle_quarantines += materialised.lifecycle_quarantines;
    }
    Ok(report)
}

fn materialise_vehicle_lifecycle(
    store: &HubStore,
    vehicle_id: Uuid,
    car_id: i64,
    received_at_ms: i64,
) -> Result<LifecycleMaterialisationReport, CollectorError> {
    let existing = store.load_lifecycle_state(vehicle_id)?;
    let mut state = match existing.as_ref() {
        Some(record) => match OpenSessionState::decode(&record.open_session_json) {
            Ok(state) => state,
            Err(_) => {
                // Corrupt open state is quarantined and rebuilt from a clean
                // cursor so prior completed history remains untouched.
                let mut clean = OpenSessionState::new();
                clean.last_observation_id = record.last_observation_id;
                clean
            }
        },
        None => OpenSessionState::new(),
    };
    // After TeslaMate import, materialised_* already holds pack-local ids
    // (position_id up to 10M+). Seed next_* so live Owner-API projection never
    // collides with imported primary keys (R06 continuity UNIQUE constraint).
    seed_lifecycle_ids_from_materialised(store, vehicle_id, &mut state)?;
    // Incremental lifecycle: do not rehydrate full open child collections here.
    // Aggregates + lifecycle_open_rows keep close/materialization correct.

    let observations = store.observations_after_id_for_vehicle(
        vehicle_id,
        state.last_observation_id,
        crate::db::MAX_OBSERVATION_QUERY_LIMIT,
    )?;

    let mut report = LifecycleMaterialisationReport::default();
    let mut total_delta = crate::lifecycle::LifecycleDelta::default();
    let mut quarantined = existing.as_ref().is_some_and(|record| record.quarantined);

    for observation in observations {
        let sample = LifecycleSample {
            observation_id: observation.observation_id,
            observed_at_ms: observation.observed_at_ms,
            vehicle_state: observation_vehicle_state(&observation.payload),
            payload: observation.payload,
        };
        let step = apply_sample(state, car_id, &sample)?;
        state = step.state;
        quarantined |= step.quarantined;
        if step.quarantined {
            report.lifecycle_quarantines += 1;
        }
        report.drives_closed += step.delta.drives.len();
        report.charges_closed += step.delta.charges.len();
        // Stationary / free positions and charge samples that close in this step.
        report.positions_materialised += step
            .delta
            .positions
            .iter()
            .filter(|position| position.drive_id.is_none())
            .count();
        report.charge_samples_materialised += 0; // closed-session samples counted below
        total_delta.drives.extend(step.delta.drives);
        for discarded_drive_id in &step.delta.discarded_drive_ids {
            total_delta
                .open_drive_positions
                .retain(|position| position.drive_id != Some(*discarded_drive_id));
        }
        total_delta
            .discarded_drive_ids
            .extend(step.delta.discarded_drive_ids);
        total_delta.positions.extend(step.delta.positions);
        total_delta.charges.extend(step.delta.charges);
        total_delta.charge_samples.extend(step.delta.charge_samples);
        total_delta.states.extend(step.delta.states);
        total_delta.updates.extend(step.delta.updates);
        total_delta
            .charge_start_coordinates
            .extend(step.delta.charge_start_coordinates);
        total_delta
            .open_drive_positions
            .extend(step.delta.open_drive_positions);
        total_delta
            .open_charge_samples
            .extend(step.delta.open_charge_samples);
    }

    if let Some(open) = state.open_drive.as_mut() {
        open.positions.clear();
    }
    if let Some(open) = state.open_charge.as_mut() {
        open.samples.clear();
    }
    // Closed drives/charges materialise durable open-row children that were never
    // kept in active memory. Count them for the materialisation report.
    for drive in &total_delta.drives {
        let mut ids = std::collections::HashSet::new();
        for position in total_delta
            .positions
            .iter()
            .filter(|position| position.drive_id == Some(drive.id))
        {
            ids.insert(position.id);
        }
        for position in total_delta
            .open_drive_positions
            .iter()
            .filter(|position| position.drive_id == Some(drive.id))
        {
            ids.insert(position.id);
        }
        for position in store.open_drive_positions(vehicle_id, drive.id)? {
            ids.insert(position.id);
        }
        report.positions_materialised += ids.len();
    }
    for charge in &total_delta.charges {
        let mut ids = std::collections::HashSet::new();
        for sample in total_delta
            .charge_samples
            .iter()
            .filter(|sample| sample.charge_process_id == charge.id)
        {
            ids.insert(sample.id);
        }
        for sample in total_delta
            .open_charge_samples
            .iter()
            .filter(|sample| sample.charge_process_id == charge.id)
        {
            ids.insert(sample.id);
        }
        for sample in store.open_charge_samples(vehicle_id, charge.id)? {
            ids.insert(sample.id);
        }
        report.charge_samples_materialised += ids.len();
    }
    let encoded = state.encode().map_err(CollectorError::Lifecycle)?;
    store.commit_lifecycle_delta(&crate::db::LifecycleCommit {
        vehicle_id,
        car_id,
        open_session_json: &encoded,
        last_observation_id: state.last_observation_id,
        quarantined,
        updated_at_ms: received_at_ms,
        delta: &total_delta,
    })?;
    Ok(report)
}

fn force_close_vehicle_for_service(
    store: &HubStore,
    source_vehicle_id: VehicleId,
    closed_at_ms: i64,
) -> Result<(), CollectorError> {
    force_close_vehicle_for_service_provider(
        store,
        source_vehicle_id,
        closed_at_ms,
        CollectorProvider::Legacy,
    )
}

fn force_close_vehicle_for_service_provider(
    store: &HubStore,
    source_vehicle_id: VehicleId,
    closed_at_ms: i64,
    provider: CollectorProvider,
) -> Result<(), CollectorError> {
    let source = store.register_source(&provider_source(provider), closed_at_ms)?;
    let registered = store.register_vehicle(
        &VehicleDescriptor::new(source.source_id, source_vehicle_id.get().to_string())
            .with_tesla_identity(Some(source_vehicle_id.get() as i64), None),
        closed_at_ms,
    )?;
    let pack_car_id =
        projection_car_id_for_vehicle(store, registered.vehicle_id, source_vehicle_id.get())?;
    let existing = store.load_lifecycle_state(registered.vehicle_id)?;
    let state = match existing.as_ref() {
        Some(record) => OpenSessionState::decode(&record.open_session_json)
            .map_err(CollectorError::Lifecycle)?,
        None => OpenSessionState::new(),
    };
    let step = force_close_for_service(state, pack_car_id, closed_at_ms)?;
    let encoded = step.state.encode().map_err(CollectorError::Lifecycle)?;
    store.commit_lifecycle_delta(&crate::db::LifecycleCommit {
        vehicle_id: registered.vehicle_id,
        car_id: pack_car_id,
        open_session_json: &encoded,
        last_observation_id: step.state.last_observation_id,
        quarantined: existing.as_ref().is_some_and(|record| record.quarantined),
        updated_at_ms: closed_at_ms,
        delta: &step.delta,
    })?;
    Ok(())
}

/// Publish a typed first-party mirror for every discovered owner vehicle.
/// Completed drive, position, charge, and charge-sample rows come only from
/// the materialised lifecycle store — never fabricated from a single sample.
fn publish_compatibility_snapshots(
    store: &HubStore,
    publication_gate: &crate::db::PublicationGate,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
    published_at_ms: i64,
) -> Result<usize, CollectorError> {
    publish_compatibility_snapshots_for_provider(
        store,
        publication_gate,
        cursor_key,
        collection,
        published_at_ms,
        CollectorProvider::Legacy,
    )
}

fn publish_compatibility_snapshots_for_provider(
    store: &HubStore,
    publication_gate: &crate::db::PublicationGate,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
    published_at_ms: i64,
    provider: CollectorProvider,
) -> Result<usize, CollectorError> {
    let source = store.register_source(&provider_source(provider), published_at_ms)?;
    let installation_id = store.installation_id()?;
    let snapshots: HashMap<u64, &VehicleData> = collection
        .snapshots
        .iter()
        .map(|snapshot| (snapshot.vehicle_id().get(), snapshot))
        .collect();
    let writer = ProjectionPackWriter::new(store.packs_dir());

    let mut published = 0;
    for vehicle in &collection.vehicles {
        let source_vehicle_id = vehicle.id.get();
        let mut descriptor =
            VehicleDescriptor::new(source.source_id, source_vehicle_id.to_string())
                .with_tesla_identity(Some(source_vehicle_id as i64), None);
        descriptor.vin = clean_optional_text(Some(&vehicle.vin));
        descriptor.display_name = clean_optional_text(vehicle.display_name.as_deref());
        let registered = store.register_vehicle(&descriptor, published_at_ms)?;
        let selected_car_id =
            projection_car_id_for_vehicle(store, registered.vehicle_id, source_vehicle_id)?;
        // Discovery can be the first live Owner-API event after a TeslaMate
        // import. Imported history lives in immutable packs and therefore does
        // not guarantee a collector-side materialised car row. Seed that row
        // before recording the settings mutation so a sparse V2 successor can
        // always resolve both mutations from durable materialised state.
        let seed_car = compatibility_car(
            vehicle,
            snapshots.get(&source_vehicle_id).copied(),
            selected_car_id,
        );
        store.persist_materialised_car_if_absent(registered.vehicle_id, &seed_car)?;
        store.upsert_car_settings(registered.vehicle_id, selected_car_id, &vehicle.settings)?;
        if store.vehicle_has_v2_base(registered.vehicle_id)? {
            if let Some(sync_claim) =
                store.claim_sync_mutations(registered.vehicle_id, published_at_ms, 10_000)?
                && let Err(error) = publish_v2_delta(store, cursor_key, &sync_claim)
            {
                store.release_sync_mutations(&sync_claim)?;
                return Err(error);
            }
            continue;
        }
        let history = store.materialised_history(registered.vehicle_id)?;
        let durable_car = match history.car.clone() {
            Some(car) => car,
            None => {
                return Err(crate::db::StoreError::SyncMutation(
                    "missing materialised car after compatibility seed".into(),
                )
                .into());
            }
        };
        let states = history.states.clone();
        let updates = history.updates.clone();
        let snapshot = ProjectionSnapshot {
            cars: vec![durable_car],
            drives: history.drives,
            positions: history.positions,
            charges: history.charges,
            charge_samples: history.charge_samples,
        };
        let fingerprint = Sha256Digest::from_bytes(
            Sha256::digest(
                serde_json::to_vec(&(&snapshot, &states, &updates))
                    .map_err(CollectorError::SerializeSnapshot)?,
            )
            .into(),
        );
        if store.snapshot_fingerprint_is_current(registered.vehicle_id, fingerprint)? {
            continue;
        }
        let sequence =
            store.reserve_next_full_snapshot_sequence(publication_gate, registered.vehicle_id)?;
        let binding = ProjectionBinding {
            installation_id,
            account_id: source.source_id,
            vehicle_id: registered.vehicle_id,
            generation: source.generation,
            selected_car_id,
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
        let built =
            writer.write_full_snapshot_with_states_and_updates(&request, &states, &updates)?;
        let manifest = request
            .signed_manifest_with_states_and_updates(&built, &states, &updates, cursor_key)?;
        store.finalize_import_snapshot_with_binding(&manifest, fingerprint, &[], &binding)?;
        published += 1;
    }
    Ok(published)
}

fn compatibility_car(
    vehicle: &crate::owner_api::Vehicle,
    snapshot: Option<&VehicleData>,
    selected_car_id: i64,
) -> ProjectionCar {
    let raw_car_type =
        snapshot.and_then(|snapshot| nested_text(snapshot, "vehicle_config", "car_type"));
    let model = raw_car_type
        .map(crate::hub_pack::normalize_tesla_model_code)
        .unwrap_or_else(|| "Tesla".to_owned());
    let trim_badging = snapshot
        .and_then(|snapshot| nested_text(snapshot, "vehicle_config", "trim_badging"))
        .map(crate::hub_pack::normalize_tesla_trim);
    ProjectionCar {
        id: selected_car_id,
        name: clean_required_text(vehicle.display_name.as_deref(), "Tesla"),
        model: model.clone(),
        vin: clean_optional_text(Some(&vehicle.vin)),
        source_eid: Some(vehicle.id.get() as i64),
        source_vid: None,
        trim_badging: trim_badging.clone(),
        marketing_name: crate::hub_pack::derive_tesla_marketing_name(
            &model,
            trim_badging.as_deref(),
            raw_car_type,
            Some(&vehicle.vin),
        ),
        exterior_color: snapshot
            .and_then(|snapshot| nested_text(snapshot, "vehicle_config", "exterior_color"))
            .map(ToOwned::to_owned),
        wheel_type: snapshot
            .and_then(|snapshot| nested_text(snapshot, "vehicle_config", "wheel_type"))
            .map(ToOwned::to_owned),
        spoiler_type: snapshot
            .and_then(|snapshot| nested_text(snapshot, "vehicle_config", "spoiler_type"))
            .map(ToOwned::to_owned),
        firmware_version: clean_optional_text(
            snapshot.and_then(|snapshot| nested_text(snapshot, "vehicle_state", "car_version")),
        ),
        efficiency_wh_per_km: None,
        settings: vehicle.settings.clone(),
    }
}

fn compatibility_car_id(source_vehicle_id: u64) -> i64 {
    // The pack contract uses a positive signed local car ID. This is only an
    // in-pack foreign key; the durable Hub identity is the registered UUID.
    i64::try_from(source_vehicle_id).expect("owner API admission bounds vehicle IDs")
}

/// Pack-local car id for live collection on a registered vehicle.
///
/// When the vehicle already has an immutable V2 base (TeslaMate import), live
/// Owner-API materialisation and deltas must reuse that base's
/// `selected_car_id` (TeslaMate car_id, e.g. 1). Using the Owner-API EID as
/// `compatibility_car_id` would create pack-local id conflicts and break
/// migration-to-Hub continuity (R06).
fn projection_car_id_for_vehicle(
    store: &HubStore,
    vehicle_id: Uuid,
    source_vehicle_id: u64,
) -> Result<i64, CollectorError> {
    if store.vehicle_has_v2_base(vehicle_id)? {
        let binding = store.v2_projection_binding(vehicle_id)?;
        if binding.selected_car_id > 0 {
            return Ok(binding.selected_car_id);
        }
    }
    Ok(compatibility_car_id(source_vehicle_id))
}

/// Raise open-session id cursors above any durable materialised history for
/// this vehicle so import-backed rows and live collection share one id space.
///
/// TeslaMate import publishes the full history as V2 packs and may only
/// materialise a subset (e.g. positions/states) into `materialised_*`. Drive /
/// charge / sample / update ids still occupy pack-local primary keys. Live
/// collection must not reuse those ids or the client V2 integrity check fails
/// when a delta upserts drive id=1 over an imported drive with different times
/// (positions fall outside the drive interval).
const LEGACY_IMPORT_MAX_ID_SQL: &str = "SELECT COALESCE(MAX(entity_id), 0)
       FROM teslamate_import_projection_rows
      WHERE vehicle_id = ?1 AND entity = ?2";
const CURRENT_IMPORT_MAX_ID_SQL: &str = "SELECT COALESCE(MAX(entity_id), 0)
       FROM teslamate_import_projection_state_rows
      WHERE vehicle_id = ?1 AND entity_ordinal = ?2";

fn seed_lifecycle_ids_from_materialised(
    store: &HubStore,
    vehicle_id: Uuid,
    state: &mut OpenSessionState,
) -> Result<(), CollectorError> {
    if state.id_cursors_seeded {
        return Ok(());
    }
    let connection = store.open().map_err(CollectorError::from)?;
    let max_i64 = |table: &str, column: &str| -> Result<i64, CollectorError> {
        let sql = format!("SELECT COALESCE(MAX({column}), 0) FROM {table} WHERE vehicle_id = ?1");
        connection
            .query_row(&sql, rusqlite::params![vehicle_id.to_string()], |row| {
                row.get(0)
            })
            .map_err(StoreError::Query)
            .map_err(CollectorError::Store)
    };
    // Tables may be empty; COALESCE handles that.
    let max_drive = max_i64("materialised_drives", "drive_id")?;
    let max_position = max_i64("materialised_positions", "position_id")?;
    let max_charge = max_i64("materialised_charges", "charge_id")?;
    let max_sample = max_i64("materialised_charge_samples", "sample_id")?;
    let max_state = max_i64("materialised_states", "state_id")?;
    let max_update = max_i64("materialised_updates", "update_id")?;

    // Both import catalogues have covering primary keys beginning with
    // (vehicle_id, entity/ordinal, entity_id). Point-range MAX queries can seek
    // directly to the final row. The former UNION/GROUP BY scanned and sorted
    // millions of imported keys on every live telemetry sample.
    let import_max = |entity: &str, ordinal: i64| -> Result<i64, CollectorError> {
        let legacy = connection
            .query_row(
                LEGACY_IMPORT_MAX_ID_SQL,
                rusqlite::params![vehicle_id.to_string(), entity],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Query)
            .map_err(CollectorError::Store)?;
        let current = connection
            .query_row(
                CURRENT_IMPORT_MAX_ID_SQL,
                rusqlite::params![vehicle_id.to_string(), ordinal],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Query)
            .map_err(CollectorError::Store)?;
        Ok(legacy.max(current))
    };
    let import_drive = import_max("drive", 1)?;
    let import_position = import_max("position", 2)?;
    let import_charge = import_max("charge", 3)?;
    let import_sample = import_max("charge_sample", 4)?;
    let import_state = import_max("state", 5)?;
    let import_update = import_max("update", 6)?;

    let max_drive = max_drive.max(import_drive);
    let max_position = max_position.max(import_position);
    let max_charge = max_charge.max(import_charge);
    let max_sample = max_sample.max(import_sample);
    let max_state = max_state.max(import_state);
    let max_update = max_update.max(import_update);

    state.next_drive_id = state.next_drive_id.max(max_drive.saturating_add(1).max(1));
    state.next_position_id = state
        .next_position_id
        .max(max_position.saturating_add(1).max(1));
    state.next_charge_id = state
        .next_charge_id
        .max(max_charge.saturating_add(1).max(1));
    state.next_charge_sample_id = state
        .next_charge_sample_id
        .max(max_sample.saturating_add(1).max(1));
    state.next_state_id = state.next_state_id.max(max_state.saturating_add(1).max(1));
    state.next_update_id = state
        .next_update_id
        .max(max_update.saturating_add(1).max(1));
    state.id_cursors_seeded = true;
    Ok(())
}

fn nested_text<'a>(snapshot: &'a VehicleData, group: &str, field: &str) -> Option<&'a str> {
    snapshot
        .fields()
        .get(group)
        .and_then(Value::as_object)
        .and_then(|fields| fields.get(field))
        .and_then(Value::as_str)
}

fn clean_required_text(value: Option<&str>, fallback: &str) -> String {
    clean_optional_text(value).unwrap_or_else(|| fallback.to_owned())
}

fn clean_optional_text(value: Option<&str>) -> Option<String> {
    const MAX_COMPATIBILITY_TEXT_BYTES: usize = 512;
    let value = value?.trim();
    (!value.is_empty()
        && value.len() <= MAX_COMPATIBILITY_TEXT_BYTES
        && !value.chars().any(char::is_control))
    .then(|| value.to_owned())
}

fn observation_payload(
    snapshot: &VehicleData,
    source_vehicle_state: &str,
    provider: CollectorProvider,
) -> Value {
    let mut payload = Map::new();
    payload.insert(
        "record_type".to_owned(),
        Value::String(provider_vehicle_data_record_type(provider).to_owned()),
    );
    payload.insert(
        "source_vehicle_id".to_owned(),
        Value::String(snapshot.vehicle_id().get().to_string()),
    );
    payload.insert(
        "source_vehicle_state".to_owned(),
        Value::String(source_vehicle_state.to_owned()),
    );
    payload.insert(
        "provider_raw_json".to_owned(),
        snapshot.provider_raw_json().clone(),
    );
    Value::Object(payload)
}

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

fn observation_timestamp(snapshot: &VehicleData, received_at_ms: i64) -> i64 {
    let fields = snapshot.fields();
    let candidates = [
        fields
            .get("drive_state")
            .and_then(Value::as_object)
            .and_then(|drive_state| drive_state.get("timestamp")),
        fields.get("timestamp"),
    ];
    let maximum = received_at_ms.saturating_add(FUTURE_TIMESTAMP_SKEW_MS);
    candidates
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .find(|timestamp| {
            (*timestamp >= EARLIEST_PLAUSIBLE_TIMESTAMP_MS) && (*timestamp <= maximum)
        })
        .unwrap_or(received_at_ms)
}

fn current_epoch_millis() -> Result<i64, CollectorError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CollectorError::SystemClockBeforeEpoch)?;
    i64::try_from(duration.as_millis()).map_err(|_| CollectorError::SystemClockOutOfRange)
}

fn assert_runtime_sensitive_access(
    admission: Option<&crate::hub_user_process::AdmittedUserHub>,
) -> Result<(), CollectorError> {
    let Some(admission) = admission else {
        return Ok(());
    };
    admission
        .assert_sensitive_access()
        .map_err(|_| CollectorError::SensitiveAccessUnavailable)
}
