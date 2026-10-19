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

/// Store discovery candidates by the rule of C3 R14 on a transaction of their
/// own (the supervised loops store them inside the tick transaction, `tick.rs`;
/// this function keeps its signature for the tests that call it), then publish
/// the compatibility snapshots as before.
#[cfg(test)]
async fn persist_discovery_events_with_timeout(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    provider: CollectorProvider,
    offline_drive_timeout: Duration,
) -> Result<(), CollectorError> {
    persist_discovery_batch(
        store,
        cursor_key,
        vehicles,
        provider,
        offline_drive_timeout,
        true,
    )
    .await
}

// A successful inventory response is current presence evidence, but R14 stores
// nothing for a state the lifecycle already holds: only a missing current row is
// written. Nothing is published for these vehicles.
#[cfg(test)]
async fn persist_unchanged_discoveries(
    store: &HubStore,
    vehicles: &[Vehicle],
    provider: CollectorProvider,
) -> Result<(), CollectorError> {
    persist_unchanged_discoveries_with_timeout(
        store,
        vehicles,
        provider,
        crate::lifecycle::DEFAULT_OFFLINE_DRIVE_TIMEOUT,
    )
    .await
}

#[cfg(test)]
async fn persist_unchanged_discoveries_with_timeout(
    store: &HubStore,
    vehicles: &[Vehicle],
    provider: CollectorProvider,
    offline_drive_timeout: Duration,
) -> Result<(), CollectorError> {
    persist_discovery_batch(
        store,
        &CursorKey::from_bytes([0; 32]),
        vehicles,
        provider,
        offline_drive_timeout,
        false,
    )
    .await
}

/// The R14 rule for a batch of candidates on one transaction of its own, with the
/// lifecycle facts read inside it (no scheduler cache). One implementation serves
/// both public functions above and the tick transaction.
#[cfg(test)]
async fn persist_discovery_batch(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    provider: CollectorProvider,
    offline_drive_timeout: Duration,
    publish: bool,
) -> Result<(), CollectorError> {
    if vehicles.is_empty() {
        return Ok(());
    }
    let publication_gate = store.acquire_publication_gate().await?;
    let observed_at_ms = current_epoch_millis()?;
    let mut identities = IdentityCache::default();
    let mut registered = HashMap::new();
    for vehicle in vehicles {
        registered.insert(
            vehicle.id,
            identities.resolve(store, provider, vehicle, observed_at_ms)?,
        );
    }
    let commit = TickCommit {
        store,
        cursor_key,
        provider,
        offline_drive_timeout,
    };
    let items: Vec<DiscoveryItem> = vehicles
        .iter()
        .map(|vehicle| DiscoveryItem {
            vehicle: vehicle.clone(),
            received_at_ms: observed_at_ms,
        })
        .collect();
    store.collector_write_transaction(
        |transaction| {
            let mut output = TickTxOutput::default();
            for item in &items {
                store_discovery_in_transaction(
                    store,
                    transaction,
                    &commit,
                    &registered[&item.vehicle.id],
                    item,
                    None,
                    &mut output,
                )?;
            }
            let wrote = output.wrote;
            Ok::<_, CollectorError>(((), wrote))
        },
        || Ok::<(), CollectorError>(()),
    )?;
    if publish {
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
    }
    Ok(())
}

fn discovery_payload(vehicle: &Vehicle, provider: CollectorProvider) -> Value {
    serde_json::json!({
        "record_type": provider_discovery_record_type(provider),
        "source_vehicle_id": vehicle.id.get().to_string(),
        "source_vehicle_state": vehicle.state,
    })
}

/// The cadence phase of a `vehicle_data` snapshot (C3 R15), first match:
/// 1. Motion (C1 R40 on this snapshot alone, gear normalised by C1 R40a: gear D,
///    R or N, or no gear and `drive_state.speed > 0`), with `service_mode` not
///    true, and Motion survives C1 R22 against plugged evidence: plugged evidence
///    (`charging_state` Charging, Starting, Stopped, NoPower or Complete) wins
///    only when both `drive_state.timestamp` and `charge_state.timestamp` are
///    integers and the charge time is later: `Driving`;
/// 2. `vehicle_state.software_update.status` trimmed and ASCII-lowercased equals
///    `installing`: `Updating`;
/// 3. `charging_state` Charging or Starting, with `service_mode` not true:
///    `Charging`;
/// 4. otherwise `Online`.
///
/// Reasons: the order and the conflict rule are C1's (C1 R21: drive over install
/// over charge; C1 R22: the later of the drive and charge statements wins and
/// Motion wins a tie), so on one snapshot the cadence follows what C1 records
/// (review N1); `Starting` gets the charging cadence because energy is about to
/// flow and the session's first samples are its start; `Stopped`, `NoPower`,
/// `Complete` and `Disconnected` move no energy, so they are parked cadence; in
/// service mode C1 opens no sessions (C1 R25), so session cadences buy nothing.
fn poll_phase(snapshot: &VehicleData) -> PollPhase {
    let fields = snapshot.fields();
    let group = |name: &str| fields.get(name).and_then(Value::as_object);
    let drive = group("drive_state");
    let charge = group("charge_state");
    let vehicle_state = group("vehicle_state");
    let in_service_mode = snapshot_service_mode(snapshot) == Some(true);
    let charging_state = charge
        .and_then(|group| group.get("charging_state"))
        .and_then(Value::as_str)
        .and_then(crate::lifecycle::normalise_enum_text);
    // Gear: only P, R, N and D are gears; every other name means no gear.
    let gear = drive
        .and_then(|group| group.get("shift_state"))
        .and_then(Value::as_str)
        .and_then(crate::lifecycle::normalise_enum_text)
        .filter(|name| matches!(*name, "P" | "R" | "N" | "D"));
    let moving = match gear {
        Some("D" | "R" | "N") => true,
        Some(_) => false,
        None => drive
            .and_then(|group| group.get("speed"))
            .and_then(Value::as_f64)
            .is_some_and(|speed| speed > 0.0),
    };
    let plugged = matches!(
        charging_state,
        Some("Charging" | "Starting" | "Stopped" | "NoPower" | "Complete")
    );
    // C1 R22: the later statement wins, and plugged evidence needs both times
    // to be integers to be judged later at all.
    let timestamp = |group: Option<&Map<String, Value>>| {
        group
            .and_then(|group| group.get("timestamp"))
            .and_then(Value::as_i64)
    };
    let plugged_wins = plugged
        && matches!(
            (timestamp(drive), timestamp(charge)),
            (Some(drive_time), Some(charge_time)) if charge_time > drive_time
        );
    if moving && !in_service_mode && !plugged_wins {
        return PollPhase::Driving;
    }
    let installing = vehicle_state
        .and_then(|group| group.get("software_update"))
        .and_then(Value::as_object)
        .and_then(|update| update.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| status.trim().eq_ignore_ascii_case("installing"));
    if installing {
        return PollPhase::Updating;
    }
    if matches!(charging_state, Some("Charging" | "Starting")) && !in_service_mode {
        return PollPhase::Charging;
    }
    PollPhase::Online
}

/// Whether a `vehicle_data` snapshot shows the car in use (C3 R29): any of
///
/// - `drive_state.shift_state` is D, R or N after C1 R40a normalisation (the
///   drivetrain is in a driving gear), or there is no gear and
///   `drive_state.speed > 0` (the car is moving). A car in service mode that is
///   moving is in use too, so motion counts whatever `service_mode` says;
/// - `charge_state.charging_state` is `Charging` or `Starting` (energy is
///   flowing or about to, and the car stays awake while charging anyway);
/// - `vehicle_state.software_update.status` trimmed and ASCII-lowercased is
///   `installing` (the car is unavailable while it installs);
/// - `climate_state.is_climate_on` is true while `climate_keeper_mode` is absent,
///   null or `"off"` (ASCII case-insensitive): the owner or a schedule is
///   conditioning the cabin, usually just before a drive, and the car stays
///   awake regardless. A keeper mode other than `"off"` (dog, camp and the like)
///   holds the cabin for hours with nobody driving, so polling through it would
///   catch nothing, for the reason sentry mode is not use either;
/// - `vehicle_state.is_user_present` or `vehicle_state.remote_start` is true (a
///   person is in the car, or keyless driving was enabled remotely: a drive is
///   imminent);
/// - `vehicle_state.locked` is false and the car's `req_not_unlocked` setting is
///   on (the owner's opt-in, C3 R58).
///
/// Not use: sentry mode (the car stays awake for recording and the Hub stores
/// nothing beyond a parked snapshot, so polling a sentry car all night would only
/// spend requests), open doors, trunks or windows (not in the data model, and a
/// door left open overnight would keep polling all night), and the parked
/// charging states `Stopped`, `NoPower`, `Complete` and `Disconnected` (no energy
/// flows; a scheduled charge wakes the car itself, which the wake rule sees). A
/// field that is absent or of the wrong JSON type gives no evidence.
fn snapshot_shows_use(snapshot: &VehicleData, req_not_unlocked: bool) -> bool {
    let fields = snapshot.fields();
    let group = |name: &str| fields.get(name).and_then(Value::as_object);
    let flag = |group: Option<&Map<String, Value>>, key: &str| {
        group
            .and_then(|group| group.get(key))
            .and_then(Value::as_bool)
    };
    let drive = group("drive_state");
    let charge = group("charge_state");
    let climate = group("climate_state");
    let vehicle_state = group("vehicle_state");
    // Gear: only P, R, N and D are gears; every other name means no gear (C1
    // R40, R40a).
    let gear = drive
        .and_then(|group| group.get("shift_state"))
        .and_then(Value::as_str)
        .and_then(crate::lifecycle::normalise_enum_text)
        .filter(|name| matches!(*name, "P" | "R" | "N" | "D"));
    let moving = match gear {
        Some("D" | "R" | "N") => true,
        Some(_) => false,
        None => drive
            .and_then(|group| group.get("speed"))
            .and_then(Value::as_f64)
            .is_some_and(|speed| speed > 0.0),
    };
    if moving {
        return true;
    }
    let charging = charge
        .and_then(|group| group.get("charging_state"))
        .and_then(Value::as_str)
        .and_then(crate::lifecycle::normalise_enum_text);
    if matches!(charging, Some("Charging" | "Starting")) {
        return true;
    }
    let installing = vehicle_state
        .and_then(|group| group.get("software_update"))
        .and_then(Value::as_object)
        .and_then(|update| update.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| status.trim().eq_ignore_ascii_case("installing"));
    if installing {
        return true;
    }
    // A keeper mode of the wrong JSON type is no statement about a keeper, so it
    // reads as absent.
    let keeper_holds_the_cabin = climate
        .and_then(|group| group.get("climate_keeper_mode"))
        .and_then(Value::as_str)
        .is_some_and(|mode| !mode.eq_ignore_ascii_case("off"));
    if flag(climate, "is_climate_on") == Some(true) && !keeper_holds_the_cabin {
        return true;
    }
    if flag(vehicle_state, "is_user_present") == Some(true)
        || flag(vehicle_state, "remote_start") == Some(true)
    {
        return true;
    }
    req_not_unlocked && flag(vehicle_state, "locked") == Some(false)
}

/// The test-only form of [`sleep_eligible_with_policy`] with the default policy
/// (an unlocked car does not count as in use, C3 R30).
#[cfg(test)]
fn sleep_eligible(snapshot: &VehicleData) -> bool {
    sleep_eligible_with_policy(snapshot, false)
}

/// Whether a read of the car may let it fall asleep (C3 R30): it shows no use
/// (R29). One definition serves both supervised loops; `req_not_unlocked` is the
/// car's own setting. Reason: the idle window of R31 must start only when the car
/// is doing nothing a poller could miss.
fn sleep_eligible_with_policy(snapshot: &VehicleData, req_not_unlocked: bool) -> bool {
    !snapshot_shows_use(snapshot, req_not_unlocked)
}

/// `vehicle_state.service_mode` when it is a JSON boolean (C3 R15), else `None`.
fn snapshot_service_mode(snapshot: &VehicleData) -> Option<bool> {
    snapshot
        .fields()
        .get("vehicle_state")
        .and_then(Value::as_object)
        .and_then(|group| group.get("service_mode"))
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
        // A live car's suspend length is final (C3 R60): no model lookup. A row an
        // earlier Hub version left unresolved gets the Hub default once, before
        // the upsert marks it resolved. Imported rows and owner-set values are
        // never changed.
        store.resolve_car_suspend_min(registered.vehicle_id)?;
        live_settings.suspend_min_resolved = true;
        store.upsert_car_settings(registered.vehicle_id, car_id, &live_settings)?;
        let latest_snapshot = collection
            .snapshots
            .iter()
            .find(|snapshot| snapshot.vehicle_id() == vehicle.id);
        let seed_car = compatibility_car(vehicle, latest_snapshot, car_id);
        store.persist_materialised_car_if_absent(registered.vehicle_id, &seed_car)?;
        let materialised =
            materialise_vehicle_lifecycle(store, registered.vehicle_id, car_id, received_at_ms)?;
        report.drives_closed += materialised.drives_closed;
        report.charges_closed += materialised.charges_closed;
        report.positions_materialised += materialised.positions_materialised;
        report.charge_samples_materialised += materialised.charge_samples_materialised;
        report.lifecycle_quarantines += materialised.lifecycle_quarantines;
    }
    Ok(report)
}

/// Count the children of the batch's closed drives and charges (C1 R88): per
/// parent, the in-batch child ids plus the stored ones, each id once. Stored
/// ids are read page by page and nothing but ids is held. Returns the position
/// count, the charge-sample count and the page statistics.
fn count_closed_children(
    store: &HubStore,
    vehicle_id: Uuid,
    delta: &crate::lifecycle::LifecycleDelta,
    page_rows: usize,
) -> Result<(usize, usize, crate::db::ClosePageStats), crate::db::StoreError> {
    let mut stats = crate::db::ClosePageStats::default();
    let mut positions = 0;
    for drive in &delta.drives {
        let mut ids = std::collections::HashSet::new();
        ids.extend(
            delta
                .positions
                .iter()
                .chain(delta.open_drive_positions.iter())
                .filter(|position| position.drive_id == Some(drive.id))
                .map(|position| position.id),
        );
        store.open_child_ids_paged(
            vehicle_id,
            crate::db::OpenChildKind::Position,
            drive.id,
            page_rows,
            &mut ids,
            &mut stats,
        )?;
        positions += ids.len();
    }
    let mut samples = 0;
    for charge in &delta.charges {
        let mut ids = std::collections::HashSet::new();
        ids.extend(
            delta
                .charge_samples
                .iter()
                .chain(delta.open_charge_samples.iter())
                .filter(|sample| sample.charge_process_id == charge.id)
                .map(|sample| sample.id),
        );
        store.open_child_ids_paged(
            vehicle_id,
            crate::db::OpenChildKind::ChargeSample,
            charge.id,
            page_rows,
            &mut ids,
            &mut stats,
        )?;
        samples += ids.len();
    }
    Ok((positions, samples, stats))
}

fn materialise_vehicle_lifecycle(
    store: &HubStore,
    vehicle_id: Uuid,
    car_id: i64,
    received_at_ms: i64,
) -> Result<LifecycleMaterialisationReport, CollectorError> {
    materialise_vehicle_lifecycle_with(store, None, vehicle_id, car_id, received_at_ms)
}

/// [`materialise_vehicle_lifecycle`] on the caller's transaction when one is
/// given (C3 R5, the tick transaction owns the commit), else on transactions of
/// its own.
fn materialise_vehicle_lifecycle_with(
    store: &HubStore,
    transaction: Option<&rusqlite::Transaction<'_>>,
    vehicle_id: Uuid,
    car_id: i64,
    received_at_ms: i64,
) -> Result<LifecycleMaterialisationReport, CollectorError> {
    let existing = match transaction {
        Some(transaction) => HubStore::lifecycle_state_in_transaction(transaction, vehicle_id)?,
        None => store.load_lifecycle_state(vehicle_id)?,
    };
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

    let observations = match transaction {
        Some(transaction) => HubStore::observations_after_in_transaction(
            transaction,
            vehicle_id,
            state.last_observation_id,
            crate::db::MAX_OBSERVATION_QUERY_LIMIT,
        )?,
        None => store.observations_after_id_for_vehicle(
            vehicle_id,
            state.last_observation_id,
            crate::db::MAX_OBSERVATION_QUERY_LIMIT,
        )?,
    };

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
        crate::lifecycle::merge_step_delta(&mut total_delta, step.delta);
    }

    if let Some(open) = state.open_drive.as_mut() {
        open.positions.clear();
    }
    if let Some(open) = state.open_charge.as_mut() {
        open.samples.clear();
    }
    // Closed drives/charges materialise durable open-row children that were never
    // kept in active memory. Count them for the materialisation report, reading
    // the stored ids a page at a time (C1 R88).
    let (positions, samples, _stats) = count_closed_children(
        store,
        vehicle_id,
        &total_delta,
        crate::lifecycle::K_CLOSE_PAGE_ROWS,
    )?;
    report.positions_materialised += positions;
    report.charge_samples_materialised += samples;
    let encoded = state.encode().map_err(CollectorError::Lifecycle)?;
    let commit = crate::db::LifecycleCommit {
        vehicle_id,
        car_id,
        open_session_json: &encoded,
        last_observation_id: state.last_observation_id,
        quarantined,
        updated_at_ms: received_at_ms,
        delta: &total_delta,
    };
    match transaction {
        Some(transaction) => {
            HubStore::commit_lifecycle_delta_in_transaction(transaction, &commit)?;
        }
        None => store.commit_lifecycle_delta(&commit)?,
    }
    Ok(report)
}

#[cfg(test)]
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

/// Production appends the event through the tick transaction (`push_service_refused_event`, C3 R52);
/// this standalone form serves the tests of the intake.
#[cfg(test)]
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
    // The refusal is a decision of the collector's own clock, so it enters the
    // projection as a stored observation and replay stays exact (C1 R15).
    append_hub_lifecycle_event(
        store,
        source.source_id,
        registered.vehicle_id,
        pack_car_id,
        closed_at_ms,
        HUB_EVENT_SERVICE_REFUSED,
        crate::lifecycle::DEFAULT_OFFLINE_DRIVE_TIMEOUT,
    )
}

/// `event` value of the Hub lifecycle event for a drive whose car has had no stored
/// observation for longer than `T_gap` (C1 R15, R43 item 4; C3 R40).
const HUB_EVENT_NO_DATA: &str = "no_data";

/// `event` value of the Hub lifecycle event for a vehicle that Tesla refused as
/// "in service" (C1 R15, R25).
const HUB_EVENT_SERVICE_REFUSED: &str = "service_refused";

/// Append one `hub_lifecycle_event_v1` observation through the ordinary intake
/// (`raw_observations`, then the projection), like a discovery (C1 R15,
/// section 7 item 3b). `observed_at_ms` is the Hub wall-clock time of the
/// decision; the projection never takes an end time from it, only the last
/// evidence of the session.
#[cfg(test)]
fn append_hub_lifecycle_event(
    store: &HubStore,
    source_id: Uuid,
    vehicle_id: Uuid,
    pack_car_id: i64,
    observed_at_ms: i64,
    event: &str,
    offline_drive_timeout: Duration,
) -> Result<(), CollectorError> {
    store.accept_owner_observation_and_lifecycle_with_offline_timeout(
        &ObservationInput {
            source_id,
            vehicle_id,
            observed_at_ms,
            payload: serde_json::json!({
                "record_type": "hub_lifecycle_event_v1",
                "event": event,
            }),
        },
        observed_at_ms,
        pack_car_id,
        offline_drive_timeout,
    )?;
    Ok(())
}

/// [`append_hub_lifecycle_event`] inside the tick transaction (C3 R5, R40, R52):
/// the intake and the projection join the caller's transaction, which owns the
/// commit.
fn append_hub_lifecycle_event_in_transaction(
    store: &HubStore,
    transaction: &rusqlite::Transaction<'_>,
    identity: &Registered,
    observed_at_ms: i64,
    event: &str,
    offline_drive_timeout: Duration,
) -> Result<(), CollectorError> {
    store.accept_owner_observation_and_lifecycle_in_transaction(
        transaction,
        &ObservationInput {
            source_id: identity.source_id,
            vehicle_id: identity.hub_id,
            observed_at_ms,
            payload: serde_json::json!({
                "record_type": "hub_lifecycle_event_v1",
                "event": event,
            }),
        },
        observed_at_ms,
        identity.pack_car_id,
        offline_drive_timeout,
    )?;
    Ok(())
}

/// Publish a typed first-party mirror for every discovered owner vehicle.
/// Completed drive, position, charge, and charge-sample rows come only from
/// the materialised lifecycle store — never fabricated from a single sample.
#[cfg(test)]
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
    publish_compatibility_snapshots_for_provider_guarded(
        store,
        publication_gate,
        cursor_key,
        collection,
        published_at_ms,
        provider,
        None,
    )
}

/// The publication of a supervised run (B2 fix round, P3b fix 2, R-P3bF2-1): the
/// source, each vehicle, the materialised car, the settings, the sync claim and its
/// release, the sequence reservation, the pack output and the snapshot's
/// finalisation are separate store mutations, each preceded by the run's write guard.
/// A refused write ends the publication with the sensitive failure and leaves any
/// claim as it is: the release is a mutation too.
fn publish_compatibility_snapshots_for_provider_guarded(
    store: &HubStore,
    publication_gate: &crate::db::PublicationGate,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
    published_at_ms: i64,
    provider: CollectorProvider,
    guard: Option<&RunWriteGuard>,
) -> Result<usize, CollectorError> {
    check_optional_write(guard, "publication-source")?;
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
        check_optional_write(guard, "publication-vehicle")?;
        let registered = store.register_vehicle(&descriptor, published_at_ms)?;
        check_optional_write(guard, "publication-car-id")?;
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
        check_optional_write(guard, "publication-seed-car")?;
        store.persist_materialised_car_if_absent(registered.vehicle_id, &seed_car)?;
        check_optional_write(guard, "publication-settings")?;
        store.upsert_car_settings(registered.vehicle_id, selected_car_id, &vehicle.settings)?;
        if store.vehicle_has_v2_base(registered.vehicle_id)? {
            check_optional_write(guard, "publication-sync-claim")?;
            if let Some(sync_claim) =
                store.claim_sync_mutations(registered.vehicle_id, published_at_ms, 10_000)?
                && let Err(error) = publish_v2_delta_guarded(store, cursor_key, &sync_claim, guard)
            {
                check_optional_write(guard, "publication-sync-release")?;
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
        check_optional_write(guard, "publication-sequence")?;
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
        check_optional_write(guard, "publication-pack")?;
        let built =
            writer.write_full_snapshot_with_states_and_updates(&request, &states, &updates)?;
        let manifest = request
            .signed_manifest_with_states_and_updates(&built, &states, &updates, cursor_key)?;
        check_optional_write(guard, "publication-finalize")?;
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
    // Imported rows need not be materialised or catalogued under these keys,
    // so the import watermarks of every source also bound the cursors
    // (C1 R84 point (a)); the maxima above are kept.
    state.raise_id_cursors(&store.lifecycle_watermark_id_floors(vehicle_id)?);
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
    let maximum = received_at_ms.saturating_add(K_SKEW_MS);
    candidates
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .find(|timestamp| (*timestamp >= K_EARLIEST_MS) && (*timestamp <= maximum))
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
