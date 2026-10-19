// SPDX-License-Identifier: AGPL-3.0-only

/// Payload of one legacy stream frame in the stored shape (C1 R37).
///
/// The shape stays the one `current_state.rs` merges over `vehicle_data`:
/// Tesla group objects under `fields`, values in Tesla's own units (mph,
/// miles), an absent value as JSON `null`. `latitude` and `longitude` repeat
/// `est_lat` and `est_lng`. The root `source_vehicle_state` is `online`
/// because Tesla sends `data:update` frames only from an awake, connected car.
/// One shape means one conversion path (`parse_sample`) for every source and
/// replay of stored frames never changes meaning. The kind of the stream's
/// `range` (rated or ideal) and of `soc` (total or usable) is not publicly
/// documented, so the stored key layout is kept for compatibility.
pub fn stream_observation_payload(update: &crate::tesla_stream::StreamUpdate) -> Value {
    let shift_state = update
        .shift_state
        .as_deref()
        .map(str::trim)
        .filter(|shift| !shift.is_empty());
    json!({
        "record_type": "tesla_stream_update_v1",
        "source_vehicle_state": "online",
        "fields": {
            "drive_state": {
                "timestamp": update.timestamp_ms,
                "speed": update.speed,
                "odometer": update.odometer,
                "elevation": update.elevation,
                "heading": update.heading,
                "est_heading": update.est_heading,
                "latitude": update.est_lat,
                "longitude": update.est_lng,
                "est_lat": update.est_lat,
                "est_lng": update.est_lng,
                "power": update.power,
                "shift_state": shift_state,
            },
            "charge_state": {
                "timestamp": update.timestamp_ms,
                "battery_level": update.soc,
                "ideal_battery_range": update.range,
                "est_battery_range": update.est_range,
            },
            "vehicle_state": {
                "timestamp": update.timestamp_ms,
                "odometer": update.odometer,
            },
        },
    })
}

/// Project one already-stored observation onto the open session.
///
/// Samples with `observation_id <= state.last_observation_id` are ignored so
/// restarts and retries stay idempotent (C1 R2).
pub fn apply_sample(
    state: OpenSessionState,
    car_id: i64,
    sample: &LifecycleSample,
) -> Result<LifecycleStep, LifecycleError> {
    apply_sample_with_offline_drive_timeout(state, car_id, sample, DEFAULT_OFFLINE_DRIVE_TIMEOUT)
}

/// Apply one observation (C1 R1 to R12). Pure: no clock, storage or logging.
///
/// The work follows the order of R8 exactly, so that a close in one domain
/// happens before an open in another: guard and parse; evidence; vehicle
/// facts; closes; update status; opens; extends; standalone position and
/// parked snapshot; phase and state interval; watermarks and cursor.
pub(crate) fn apply_sample_with_offline_drive_timeout(
    mut state: OpenSessionState,
    car_id: i64,
    sample: &LifecycleSample,
    offline_drive_timeout: Duration,
) -> Result<LifecycleStep, LifecycleError> {
    if car_id <= 0 {
        return Err(LifecycleError::InvalidCarId);
    }
    let t_gap_ms = i64::try_from(offline_drive_timeout.as_millis())
        .ok()
        .filter(|millis| *millis > 0)
        .ok_or(LifecycleError::InvalidOfflineDriveTimeout)?;

    // R2: a re-delivered observation is a no-op.
    if sample.observation_id <= state.last_observation_id {
        return Ok(LifecycleStep {
            state,
            delta: LifecycleDelta::default(),
            quarantined: false,
        });
    }

    // R12: an old blob's pending seed becomes a parked snapshot, once.
    convert_pending_seed(&mut state);

    // R3, R4: an unusable known record is quarantined, an unknown record or
    // an unknown Hub event only advances the cursor.
    let mut parsed = match parse_sample(sample) {
        Ok(parsed) => parsed,
        Err(LifecycleError::InvalidPayload) => {
            state.last_observation_id = sample.observation_id;
            return Ok(LifecycleStep {
                state,
                delta: LifecycleDelta::default(),
                quarantined: true,
            });
        }
        Err(error) => return Err(error),
    };
    if parsed.kind == SourceKind::Unknown
        || (parsed.kind == SourceKind::HubEvent && parsed.hub_event.is_none())
    {
        state.last_observation_id = sample.observation_id;
        return Ok(LifecycleStep {
            state,
            delta: LifecycleDelta::default(),
            quarantined: false,
        });
    }

    // R5 to R7: fresh families and reachability evidence. Freshness reads the
    // watermarks only, so it is judged before the carry-forward moves any
    // last known value.
    let fresh = fresh_families(&state, &parsed);
    carry_forward(&mut state, &mut parsed, fresh);
    let reach = accepted_reach(&state, &parsed, &sample.vehicle_state, fresh);

    // R8 step 2: evidence, then R22 conflict resolution.
    let mut drive = classify_drive(&parsed, fresh.motion);
    let mut charge = classify_charge(&parsed, fresh.charge_session);
    let mut status = if fresh.vehicle_status && parsed.kind.has_vehicle_data() {
        update_status_class(parsed.software_update_status.as_deref())
    } else {
        UpdateStatusClass::NoInfo
    };
    resolve_conflicts(&parsed, &mut drive, &mut charge, &mut status);

    let observation = Observation {
        parsed: &parsed,
        car_id,
        t_gap_ms,
        reach,
        fresh,
        drive,
        charge,
        status,
    };
    let mut delta = LifecycleDelta::default();
    apply_lifecycle_steps(&mut state, &observation, &mut delta)?;

    // R8 step 10: watermarks, last observed time, last reach, cursor.
    advance_watermarks(&mut state, &observation);
    state.last_observation_id = sample.observation_id;
    Ok(LifecycleStep {
        state,
        delta,
        quarantined: false,
    })
}

/// Convert the pending seed of an old blob into a parked snapshot (C1 R12).
///
/// The seed holds the parked values before a gap, which is what the after-gap
/// charge (R70) needs, so a charge in progress at upgrade can still be
/// recorded. With a drive open or a snapshot already present the seed is only
/// dropped: the drive close builds a snapshot, and an existing one is newer.
fn convert_pending_seed(state: &mut OpenSessionState) {
    let Some(seed) = state.pending_gained_range_charge.take() else {
        return;
    };
    if state.open_drive.is_some() || state.parked_snapshot.is_some() {
        return;
    }
    state.parked_snapshot = Some(ParkedSnapshot {
        at_ms: Some(seed.start_date_ms),
        seen_at_ms: seed.start_date_ms,
        battery_level: seed.start_battery_level,
        ideal_range_km: seed.start_ideal_range_km,
        rated_range_km: seed.start_rated_range_km,
        odometer_km: None,
        odometer_coarse: false,
        latitude: seed.start_latitude,
        longitude: seed.start_longitude,
        energy_added_kwh: Some(seed.first_energy_added),
        after_drive: false,
        unplugged: false,
    });
}

/// Carry the last known thermal and comfort values forward (C1 R39, retained).
///
/// A parsed observation takes every field it carries; fields it does not carry
/// follow the last known value, so stream frames and sparse sources do not
/// empty position rows. The last known values themselves move only for a field
/// whose family is fresh (R6a): a late-delivered observation still states its
/// own values on its own row, but must not move a "last" value backwards, or
/// a later frame that omits the field would inherit the older value.
fn carry_forward(state: &mut OpenSessionState, parsed: &mut ParsedSample, fresh: Families) {
    /// Fill a row field from the cache, and move the cache only when the
    /// observation carried the field and its family is fresh.
    fn carry<T: Copy>(row: &mut Option<T>, cache: &mut Option<T>, family_fresh: bool) {
        let carried = *row;
        *row = carried.or(*cache);
        if family_fresh && let Some(value) = carried {
            *cache = Some(value);
        }
    }
    // R6a: `battery_heater` and `battery_heater_no_power` are climate facts,
    // `battery_heater_on` a charge_session fact, `est_battery_range_km` a
    // battery fact, `tpms_pressure_*` vehicle_status facts, the rest climate.
    carry(
        &mut parsed.battery_heater,
        &mut state.last_position_battery_heater,
        fresh.climate,
    );
    carry(
        &mut parsed.battery_heater_on,
        &mut state.last_position_battery_heater_on,
        fresh.charge_session,
    );
    carry(
        &mut parsed.battery_heater_no_power,
        &mut state.last_position_battery_heater_no_power,
        fresh.climate,
    );
    carry(
        &mut parsed.est_range_km,
        &mut state.last_position_est_battery_range_km,
        fresh.battery,
    );
    carry(
        &mut parsed.fan_status,
        &mut state.last_position_fan_status,
        fresh.climate,
    );
    carry(
        &mut parsed.driver_temp_setting,
        &mut state.last_position_driver_temp_setting,
        fresh.climate,
    );
    carry(
        &mut parsed.passenger_temp_setting,
        &mut state.last_position_passenger_temp_setting,
        fresh.climate,
    );
    carry(
        &mut parsed.is_rear_defroster_on,
        &mut state.last_position_is_rear_defroster_on,
        fresh.climate,
    );
    carry(
        &mut parsed.is_front_defroster_on,
        &mut state.last_position_is_front_defroster_on,
        fresh.climate,
    );
    carry(
        &mut parsed.tpms_pressure_fl,
        &mut state.last_position_tpms_pressure_fl,
        fresh.vehicle_status,
    );
    carry(
        &mut parsed.tpms_pressure_fr,
        &mut state.last_position_tpms_pressure_fr,
        fresh.vehicle_status,
    );
    carry(
        &mut parsed.tpms_pressure_rl,
        &mut state.last_position_tpms_pressure_rl,
        fresh.vehicle_status,
    );
    carry(
        &mut parsed.tpms_pressure_rr,
        &mut state.last_position_tpms_pressure_rr,
        fresh.vehicle_status,
    );
}

/// A fact time is fresh when it is later than the family's watermark; equal
/// times are duplicates because Tesla re-serves cached groups unchanged with
/// the same timestamp (C1 R6).
fn later_than(fact_ms: i64, watermark: Option<i64>) -> bool {
    watermark.is_none_or(|watermark| fact_ms > watermark)
}

/// Which families this observation carries and which of them are fresh
/// (C1 R6). Freshness is judged per family with its own watermark, so a
/// source that carries only part of a group (the stream's battery-only
/// `charge_state`) never makes another source's other fields stale.
///
/// Old blobs have no watermark for the position, battery and odometer
/// families; they fall back to the retained group watermark (R6 table). The
/// imported gates (R6) additionally need drive facts and charge facts to be
/// later than the imported source's watermark.
fn fresh_families(state: &OpenSessionState, parsed: &ParsedSample) -> Families {
    let carried = parsed.carried;
    let drive_gate = later_than(parsed.drive_timestamp_ms, state.imported_drive_watermark_ms);
    let charge_gate = later_than(
        parsed.charge_timestamp_ms,
        state.imported_charge_watermark_ms,
    );
    Families {
        motion: carried.motion
            && drive_gate
            && later_than(parsed.drive_timestamp_ms, state.last_drive_timestamp_ms),
        position: carried.position
            && drive_gate
            && later_than(
                parsed.drive_timestamp_ms,
                state
                    .last_position_fact_ms
                    .or(state.last_drive_timestamp_ms),
            ),
        battery: carried.battery
            && later_than(
                parsed.charge_timestamp_ms,
                state
                    .last_battery_fact_ms
                    .or(state.last_charge_timestamp_ms),
            ),
        charge_session: carried.charge_session
            && charge_gate
            && later_than(parsed.charge_timestamp_ms, state.last_charge_timestamp_ms),
        climate: carried.climate
            && later_than(parsed.climate_timestamp_ms, state.last_climate_fact_ms),
        odometer: carried.odometer
            && later_than(
                parsed.vehicle_timestamp_ms,
                state
                    .last_odometer_fact_ms
                    .or(state.last_vehicle_timestamp_ms),
            ),
        vehicle_status: carried.vehicle_status
            && later_than(parsed.vehicle_timestamp_ms, state.last_vehicle_timestamp_ms),
    }
}

/// Reachability evidence of one observation (C1 R7), or `None`.
///
/// Accepted only when the observation is not older than the newest one seen
/// and is later than the imported state watermark. Data from a connected car
/// proves `online` whatever a stale envelope says; without fresh data, and for
/// discovery, the envelope text is the evidence. A Hub event gives none.
fn accepted_reach(
    state: &OpenSessionState,
    parsed: &ParsedSample,
    envelope_state: &str,
    fresh: Families,
) -> Option<Reach> {
    if parsed.kind == SourceKind::HubEvent {
        return None;
    }
    let not_older = state
        .last_observed_at_ms
        .is_none_or(|observed| parsed.observed_at_ms >= observed);
    let after_import = later_than(parsed.observed_at_ms, state.imported_state_watermark_ms);
    if !(not_older && after_import) {
        return None;
    }
    if parsed.kind.has_vehicle_data() && fresh.any() {
        return Some(Reach::Online);
    }
    Reach::from_text(envelope_state)
}

/// Resolve conflicting signals of one observation before any close or open
/// (C1 R22).
///
/// The car cannot be shifted out of Park with the cable connected, so one of
/// Motion and plugged evidence is stale: the later group time wins and, on a
/// tie, Motion wins because gear is the drivetrain's own state while
/// `charge_state` may be a cached group. The loser is discarded for the rest
/// of the observation. An Installing status together with surviving Motion is
/// discarded too, because a car cannot drive while installing (R31).
fn resolve_conflicts(
    parsed: &ParsedSample,
    drive: &mut DriveEvidence,
    charge: &mut ChargeEvidence,
    status: &mut UpdateStatusClass,
) {
    if *drive == DriveEvidence::Motion && charge.is_plugged() {
        if parsed.drive_timestamp_ms >= parsed.charge_timestamp_ms {
            *charge = ChargeEvidence::None;
        } else {
            *drive = DriveEvidence::NoInfo;
        }
    }
    if *status == UpdateStatusClass::Installing && *drive == DriveEvidence::Motion {
        *status = UpdateStatusClass::NoInfo;
    }
}

/// Everything steps 3 to 9 of one observation need, computed in steps 1 and 2
/// (C1 R8). Parts D and C read it in their hooks.
struct Observation<'a> {
    parsed: &'a ParsedSample,
    car_id: i64,
    /// `T_gap` in milliseconds, always positive.
    t_gap_ms: i64,
    /// Accepted reachability (R7).
    reach: Option<Reach>,
    /// Families that are carried and fresh (R6).
    fresh: Families,
    /// Drive evidence after conflict resolution (R40, R22).
    drive: DriveEvidence,
    /// Charge evidence after conflict resolution (R60, R22).
    charge: ChargeEvidence,
    /// Update status class after conflict resolution (R29, R22).
    status: UpdateStatusClass,
}

impl Observation<'_> {
    /// The observation's own time (R5).
    fn t_obs(&self) -> i64 {
        self.parsed.observed_at_ms
    }

    fn event(&self) -> Option<HubEventKind> {
        self.parsed.hub_event
    }
}

/// The ten steps of one observation after parsing (C1 R8), steps 2 to 9. Step 1
/// and step 10 are in the caller. Every step is wired; the drive and charge
/// steps are hooks owned by parts D and C.
fn apply_lifecycle_steps(
    state: &mut OpenSessionState,
    observation: &Observation<'_>,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    // Step 3: vehicle facts.
    if let Some(newer) = observation.parsed.car_metadata.as_ref() {
        state
            .car_metadata
            .get_or_insert_with(ProjectionCarPatch::default)
            .merge_newer(newer);
    }
    apply_service_mode(state, observation);
    apply_version_facts(state, observation, delta)?;

    // Step 4: closes, drive then charge.
    drive_close_step(state, observation, delta)?;
    charge_close_step(state, observation, delta)?;
    // R27: offline closes nothing by itself. The flag on an open drive is
    // informational only; the gap rules judge the drive on the next evidence.
    if observation.reach == Some(Reach::Offline)
        && let Some(drive) = state.open_drive.as_mut()
    {
        drive.saw_offline = true;
    }

    // Step 5: update status.
    apply_update_status(state, observation)?;

    // Step 6: opens, drive then charge.
    drive_open_step(state, observation, delta)?;
    charge_open_step(state, observation, delta)?;

    // Step 7: extends.
    drive_fold_step(state, observation, delta)?;
    charge_sample_step(state, observation, delta)?;

    // Step 8: standalone position, parked snapshot.
    standalone_position_step(state, observation, delta)?;
    parked_snapshot_step(state, observation);

    // Step 9: phase and state interval.
    let phase = derive_phase(state, observation);
    state.phase = phase;
    if observation.reach.is_some() {
        update_state_interval(state, observation.car_id, phase, observation.t_obs(), delta)?;
    }
    Ok(())
}

/// Service mode (C1 R25), step 3.
///
/// A fresh vehicle_status family from a full or sparse source that states a
/// boolean sets the flag. A full source that reports `vehicle_state` without
/// the field cannot sustain the flag and clears it; a sparse source without
/// the field is no statement. A `service_refused` Hub event sets the flag and
/// remembers its origin; the first later full-source answer with any fresh
/// family clears a refusal, because Tesla serving `vehicle_data` again shows
/// the refusal has ended. In service mode gear and charge signals reflect
/// technicians' procedures, not the owner's use (the hooks act on the flag).
fn apply_service_mode(state: &mut OpenSessionState, observation: &Observation<'_>) {
    let parsed = observation.parsed;
    if observation.event() == Some(HubEventKind::ServiceRefused) {
        state.service_mode = Some(true);
        state.service_mode_refused = true;
        return;
    }
    let status_fresh = observation.fresh.vehicle_status && parsed.kind.has_vehicle_data();
    if status_fresh && let Some(stated) = parsed.service_mode {
        state.service_mode = Some(stated);
        state.service_mode_refused = false;
        return;
    }
    if parsed.kind == SourceKind::Full && status_fresh {
        state.service_mode = None;
        state.service_mode_refused = false;
        return;
    }
    if state.service_mode_refused && parsed.kind == SourceKind::Full && observation.fresh.any() {
        state.service_mode = None;
        state.service_mode_refused = false;
    }
}

/// Version facts (C1 R31 items 1 to 4), step 3, on a fresh vehicle_status
/// family from a full source. A sparse source's version is ignored (R32):
/// Fleet Telemetry reported the available, not the installed, version before
/// firmware 2024.44.
///
/// A first comparable version is adopted without a row. A higher one is the
/// certain event that an update finished: its row starts at the open install's
/// start, or at the last time the older version was seen (a lower bound, the
/// update was missed while unobserved), and ends at the vehicle-group time. An
/// equal one only refreshes the time. A lower one is far more likely a lagging
/// or cached source than a rollback, so it is ignored, which keeps rows
/// idempotent when sources interleave.
fn apply_version_facts(
    state: &mut OpenSessionState,
    observation: &Observation<'_>,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    let parsed = observation.parsed;
    if parsed.kind != SourceKind::Full || !observation.fresh.vehicle_status {
        return Ok(());
    }
    let Some(text) = parsed.car_version.as_deref().map(str::trim) else {
        return Ok(());
    };
    let Some(key) = version_key(text) else {
        return Ok(());
    };
    let t_vehicle = parsed.vehicle_timestamp_ms;
    let known_key = state.last_car_version.as_deref().and_then(version_key);
    let Some(known_key) = known_key else {
        state.last_car_version = Some(text.to_owned());
        state.last_car_version_at_ms = Some(t_vehicle);
        return Ok(());
    };
    match compare_version_keys(&key, &known_key) {
        std::cmp::Ordering::Greater => {
            let (id, install_start) = match state.open_update.take() {
                Some(open) => (open.id, Some(open.start_date_ms)),
                None => {
                    let id = state.next_update_id;
                    state.next_update_id = id
                        .checked_add(1)
                        .ok_or(LifecycleError::IdentifierExhausted)?;
                    (id, None)
                }
            };
            let start = install_start
                .or(state.last_car_version_at_ms)
                .unwrap_or(t_vehicle)
                .min(t_vehicle);
            delta.updates.push(ProjectionUpdate {
                id,
                car_id: observation.car_id,
                start_date_ms: start,
                end_date_ms: t_vehicle,
                version: text.to_owned(),
            });
            state.last_car_version = Some(text.to_owned());
            state.last_car_version_at_ms = Some(t_vehicle);
        }
        std::cmp::Ordering::Equal => {
            state.last_car_version_at_ms = Some(t_vehicle);
        }
        std::cmp::Ordering::Less => {}
    }
    Ok(())
}

/// Update status (C1 R31 item 5), step 5, after the closes.
///
/// Installing opens an update only while no drive is open: an install cannot
/// start while a drive is still open, so the item waits for the drive closes
/// of step 4. Offered or Idle with an open update clears it without a row
/// (cancelled, failed, or not yet reporting the new version; a later increase
/// is still recorded by item 2 with a lower-bound start). Surviving Motion
/// clears it too, because a car cannot drive while installing.
fn apply_update_status(
    state: &mut OpenSessionState,
    observation: &Observation<'_>,
) -> Result<(), LifecycleError> {
    if observation.drive == DriveEvidence::Motion && state.open_update.is_some() {
        state.open_update = None;
        return Ok(());
    }
    match observation.status {
        UpdateStatusClass::Installing
            if state.open_update.is_none() && state.open_drive.is_none() =>
        {
            let id = state.next_update_id;
            state.next_update_id = id
                .checked_add(1)
                .ok_or(LifecycleError::IdentifierExhausted)?;
            state.open_update = Some(OpenUpdate {
                id,
                car_id: observation.car_id,
                start_date_ms: observation.parsed.vehicle_timestamp_ms,
            });
        }
        UpdateStatusClass::Offered | UpdateStatusClass::Idle => {
            state.open_update = None;
        }
        _ => {}
    }
    Ok(())
}

/// Phase derivation and precedence (C1 R21), step 9.
///
/// Reachability comes first, because `states` records reachability and an
/// unreachable car is unreachable whatever it was doing (asleep cannot coexist
/// with a session, R26). A drive beats an install, because Tesla does not let
/// a car drive while installing and motion is direct drivetrain evidence; an
/// install beats a charge, because the car is unavailable while installing.
/// With no evidence at all the phase stays as it was, which is the seeded
/// phase of an imported session until the first live evidence.
fn derive_phase(state: &OpenSessionState, observation: &Observation<'_>) -> VehiclePhase {
    let last_reach = observation
        .reach
        .or_else(|| state.last_reach.as_deref().and_then(Reach::from_text));
    match last_reach {
        Some(Reach::Asleep) => return VehiclePhase::Asleep,
        Some(Reach::Offline) => return VehiclePhase::Offline,
        _ => {}
    }
    if state.open_drive.is_some() {
        VehiclePhase::Driving
    } else if state.open_update.is_some() {
        VehiclePhase::Updating
    } else if state.open_charge.is_some() {
        VehiclePhase::Charging
    } else if last_reach == Some(Reach::Online) {
        VehiclePhase::Online
    } else {
        state.phase.effective()
    }
}

/// Step 10 (C1 R6, R7): each fresh family's watermark becomes the fact time,
/// the newest observation time and the accepted reach are persisted. A Hub
/// event advances no fact watermark and not the observation time, because it
/// is not an observation of the vehicle.
fn advance_watermarks(state: &mut OpenSessionState, observation: &Observation<'_>) {
    let parsed = observation.parsed;
    let fresh = observation.fresh;
    if fresh.motion {
        state.last_drive_timestamp_ms = state
            .last_drive_timestamp_ms
            .max(Some(parsed.drive_timestamp_ms));
    }
    if fresh.position {
        let watermark = state
            .last_position_fact_ms
            .or(state.last_drive_timestamp_ms);
        state.last_position_fact_ms = watermark.max(Some(parsed.drive_timestamp_ms));
    }
    if fresh.battery {
        let watermark = state
            .last_battery_fact_ms
            .or(state.last_charge_timestamp_ms);
        state.last_battery_fact_ms = watermark.max(Some(parsed.charge_timestamp_ms));
    }
    if fresh.charge_session {
        state.last_charge_timestamp_ms = state
            .last_charge_timestamp_ms
            .max(Some(parsed.charge_timestamp_ms));
    }
    if fresh.climate {
        state.last_climate_fact_ms = state
            .last_climate_fact_ms
            .max(Some(parsed.climate_timestamp_ms));
    }
    if fresh.odometer {
        let watermark = state
            .last_odometer_fact_ms
            .or(state.last_vehicle_timestamp_ms);
        state.last_odometer_fact_ms = watermark.max(Some(parsed.vehicle_timestamp_ms));
    }
    if fresh.vehicle_status {
        state.last_vehicle_timestamp_ms = state
            .last_vehicle_timestamp_ms
            .max(Some(parsed.vehicle_timestamp_ms));
    }
    if parsed.kind != SourceKind::HubEvent {
        state.last_observed_at_ms = state.last_observed_at_ms.max(Some(parsed.observed_at_ms));
    }
    if let Some(reach) = observation.reach {
        state.last_reach = Some(reach.as_str().to_owned());
    }
}

/// Apply a contiguous observation page, preserving intermediate deltas.
pub fn apply_samples(
    mut state: OpenSessionState,
    car_id: i64,
    samples: &[LifecycleSample],
) -> Result<LifecycleStep, LifecycleError> {
    let mut total = LifecycleDelta::default();
    let mut quarantined = false;
    for sample in samples {
        let step = apply_sample(state, car_id, sample)?;
        state = step.state;
        quarantined |= step.quarantined;
        merge_step_delta(&mut total, step.delta);
    }
    Ok(LifecycleStep {
        state,
        delta: total,
        quarantined,
    })
}

/// Merge the delta of one step into the delta of a batch (C1 R11, R48, R48a).
///
/// This is the one merge: `apply_samples`, the owner, edge and stream intake
/// loops of the store and the collector's materialisation loop all call it, so
/// they place children the same way. Children of a drive that is discarded in
/// this step, and children of a closed drive dated after its end (the
/// stationary tail cut off by R44), are moved to standalone positions
/// (`drive_id` `None`) and never dropped: the Hub stores every position
/// losslessly, and the observations behind them are pruned after commit.
pub(crate) fn merge_step_delta(total: &mut LifecycleDelta, step: LifecycleDelta) {
    let LifecycleDelta {
        drives,
        discarded_drive_ids,
        positions,
        charges,
        charge_samples,
        states,
        updates,
        charge_start_coordinates,
        open_drive_positions,
        open_charge_samples,
    } = step;
    total.open_drive_positions.extend(open_drive_positions);
    if !drives.is_empty() || !discarded_drive_ids.is_empty() {
        let placed = std::mem::take(&mut total.open_drive_positions);
        for mut position in placed {
            let standalone = position.drive_id.is_some_and(|drive_id| {
                discarded_drive_ids.contains(&drive_id)
                    || drives
                        .iter()
                        .any(|drive| drive.id == drive_id && position.date_ms > drive.end_date_ms)
            });
            if standalone {
                position.drive_id = None;
                total.positions.push(position);
            } else {
                total.open_drive_positions.push(position);
            }
        }
    }
    total.positions.extend(positions);
    total.drives.extend(drives);
    total.discarded_drive_ids.extend(discarded_drive_ids);
    total.charges.extend(charges);
    total.charge_samples.extend(charge_samples);
    total.states.extend(states);
    total.updates.extend(updates);
    total
        .charge_start_coordinates
        .extend(charge_start_coordinates);
    total.open_charge_samples.extend(open_charge_samples);
}

/// Close the open drive for lack of evidence, as the `no_data` Hub event does
/// (C1 R15). Equal to `apply_sample` of a `no_data` event at `closed_at_ms`,
/// except that the cursor is left alone because there is no observation id.
/// Production code appends the event as a stored observation instead, so the
/// decision is replayable; this function exists for callers that have none.
pub fn force_close_open_sessions(
    state: OpenSessionState,
    car_id: i64,
    closed_at_ms: i64,
) -> Result<LifecycleStep, LifecycleError> {
    apply_hub_event(state, car_id, closed_at_ms, "no_data")
}

/// Close open sessions because Tesla refused `vehicle_data` as "in service"
/// (C1 R15, R25). Equal to `apply_sample` of a `service_refused` event at
/// `closed_at_ms`, except that the cursor is left alone.
pub fn force_close_for_service(
    state: OpenSessionState,
    car_id: i64,
    closed_at_ms: i64,
) -> Result<LifecycleStep, LifecycleError> {
    apply_hub_event(state, car_id, closed_at_ms, "service_refused")
}

fn apply_hub_event(
    state: OpenSessionState,
    car_id: i64,
    at_ms: i64,
    event: &str,
) -> Result<LifecycleStep, LifecycleError> {
    let cursor = state.last_observation_id;
    let sample = LifecycleSample {
        observation_id: cursor
            .checked_add(1)
            .ok_or(LifecycleError::IdentifierExhausted)?,
        observed_at_ms: at_ms,
        vehicle_state: "unknown".to_owned(),
        payload: json!({"record_type": HUB_EVENT_RECORD_TYPE, "event": event}),
    };
    let mut step = apply_sample(state, car_id, &sample)?;
    step.state.last_observation_id = cursor;
    Ok(step)
}

/// Record type of a decision the collector makes from its own clock (C1 R15).
/// It enters the projection as a stored observation so replay stays exact.
const HUB_EVENT_RECORD_TYPE: &str = "hub_lifecycle_event_v1";

/// What kind of record an observation is (C1 R3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SourceKind {
    /// Owner API or Fleet API `vehicle_data` without a `provider_feed` marker.
    Full,
    /// Fleet API `vehicle_data` with a `provider_feed` marker (R76): an absent
    /// or null key is no statement.
    Sparse,
    /// A legacy stream frame (R37).
    Stream,
    /// A discovery of the vehicle list: reachability only.
    Discovery,
    /// A decision of the collector (R15).
    HubEvent,
    /// Any other record type: only the cursor advances.
    #[default]
    Unknown,
}

impl SourceKind {
    /// Whether the record carries a vehicle-data object and so can prove the
    /// car is connected.
    fn has_vehicle_data(self) -> bool {
        matches!(self, Self::Full | Self::Sparse | Self::Stream)
    }
}

/// A decision the collector made from its own clock (C1 R15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HubEventKind {
    /// No data for longer than the collector tolerates: closes an open drive.
    NoData,
    /// Tesla refused `vehicle_data` as "in service": service mode.
    ServiceRefused,
}

/// A set of fact families (C1 R6), used for "carried" and for "fresh".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Families {
    motion: bool,
    position: bool,
    battery: bool,
    charge_session: bool,
    climate: bool,
    odometer: bool,
    vehicle_status: bool,
}

impl Families {
    fn any(self) -> bool {
        self.motion
            || self.position
            || self.battery
            || self.charge_session
            || self.climate
            || self.odometer
            || self.vehicle_status
    }
}

/// One observation read into the unit's own units and ranges (C1 R4a, R5,
/// R34 to R36, R79). Shared with the charge observers through
/// `observe_charge_aggregate`.
#[derive(Debug, Default)]
struct ParsedSample {
    kind: SourceKind,
    hub_event: Option<HubEventKind>,
    /// `fleet_api_vehicle_data_v1` marker text, set exactly for sparse sources.
    provider_feed: Option<String>,
    observed_at_ms: i64,
    drive_timestamp_ms: i64,
    charge_timestamp_ms: i64,
    climate_timestamp_ms: i64,
    vehicle_timestamp_ms: i64,
    /// Families that have at least one value after R4 (R6).
    carried: Families,
    vehicle_state_present: bool,
    service_mode: Option<bool>,
    car_version: Option<String>,
    car_metadata: Option<ProjectionCarPatch>,
    shift_state: Option<String>,
    /// The `shift_state` key is present with JSON null (R40).
    shift_state_key_present: bool,
    stream_frame: bool,
    drive_data_present: bool,
    speed: Option<i64>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    power: Option<f64>,
    battery_level: Option<i64>,
    usable_battery_level: Option<i64>,
    rated_range_km: Option<f64>,
    ideal_range_km: Option<f64>,
    est_range_km: Option<f64>,
    odometer: Option<f64>,
    elevation: Option<i64>,
    is_climate_on: Option<bool>,
    inside_temp: Option<f64>,
    outside_temp: Option<f64>,
    fan_status: Option<i64>,
    driver_temp_setting: Option<f64>,
    passenger_temp_setting: Option<f64>,
    is_rear_defroster_on: Option<bool>,
    is_front_defroster_on: Option<bool>,
    tpms_pressure_fl: Option<f64>,
    tpms_pressure_fr: Option<f64>,
    tpms_pressure_rl: Option<f64>,
    tpms_pressure_rr: Option<f64>,
    charging_state: Option<String>,
    /// `software_update.status` as stated; the empty string is the idle value.
    software_update_status: Option<String>,
    charge_data_present: bool,
    charge_energy_added: Option<f64>,
    charger_power_kw: Option<f64>,
    charger_voltage: Option<f64>,
    charger_actual_current: Option<f64>,
    charger_pilot_current: Option<f64>,
    charger_phases: Option<i64>,
    fast_charger_present: Option<bool>,
    fast_charger_brand: Option<String>,
    fast_charger_type: Option<String>,
    charge_cable: Option<String>,
    battery_heater_on: Option<bool>,
    battery_heater: Option<bool>,
    battery_heater_no_power: Option<bool>,
    not_enough_power_to_heat: Option<bool>,
}

/// Smallest and largest value of the public `smallint` columns (C1 R35).
const SMALLINT_MIN: i64 = -32_768;
const SMALLINT_MAX: i64 = K_SMALLINT_MAX;

/// The data object of a vehicle-data record (C1 R3): `provider_raw_json.response`
/// when that key exists, else `vehicle_data`. Anything but an object is
/// unusable.
fn vehicle_data_object(root: &Map<String, Value>) -> Result<&Map<String, Value>, LifecycleError> {
    let provider_response = root
        .get("provider_raw_json")
        .and_then(Value::as_object)
        .and_then(|raw| raw.get("response"));
    provider_response
        .or_else(|| root.get("vehicle_data"))
        .and_then(Value::as_object)
        .ok_or(LifecycleError::InvalidPayload)
}

/// One group of the data object (C1 R4). A present group that is not a JSON
/// object makes the whole record unusable; a missing or null group is simply
/// not carried, so a source that leaves a group out is not quarantined.
fn group_object<'a>(
    data: Option<&'a Map<String, Value>>,
    key: &str,
) -> Result<Option<&'a Map<String, Value>>, LifecycleError> {
    match data.and_then(|fields| fields.get(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(group)) => Ok(Some(group)),
        Some(_) => Err(LifecycleError::InvalidPayload),
    }
}

/// A numeric field scaled to `precision` decimals and range-checked (C1 R4a,
/// R35, R36): read, round half away from zero, then check `min..=max`.
fn scaled_field(
    map: Option<&Map<String, Value>>,
    key: &str,
    precision: u32,
    min: f64,
    max: f64,
) -> Option<f64> {
    let value = round_to_precision(float_field(map?, key)?, precision);
    (min..=max).contains(&value).then_some(value)
}

/// A distance field in miles converted to kilometres and rounded to
/// `precision` decimals, range-checked (C1 R34, R35).
fn miles_field(
    map: Option<&Map<String, Value>>,
    key: &str,
    precision: u32,
    min: f64,
    max: f64,
) -> Option<f64> {
    let value = miles_to_km_rounded(float_field(map?, key)?, precision);
    (min..=max).contains(&value).then_some(value)
}

/// An integer field rounded half away from zero and range-checked (C1 R4a, R35).
fn ranged_int_field(
    map: Option<&Map<String, Value>>,
    key: &str,
    min: i64,
    max: i64,
) -> Option<i64> {
    int_field(map?, key).filter(|value| (min..=max).contains(value))
}

/// Trimmed text, `None` when empty or longer than the varchar(255) columns
/// allow (C1 R36).
fn bounded_text_field(map: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    text_field(map, key).filter(|text| text.chars().count() <= MAX_FIELD_TEXT_CHARS)
}

/// A group time (C1 R5): the group's `timestamp` when it lies within
/// `[K_EARLIEST_MS, t_obs + K_SKEW_MS]`, otherwise the observation time.
pub(crate) fn group_time(group: Option<&Map<String, Value>>, observed_at_ms: i64) -> i64 {
    let latest = observed_at_ms.saturating_add(K_SKEW_MS);
    group
        .and_then(|fields| timestamp_field(fields, "timestamp"))
        .filter(|timestamp| (K_EARLIEST_MS..=latest).contains(timestamp))
        .unwrap_or(observed_at_ms)
}

fn parse_sample(sample: &LifecycleSample) -> Result<ParsedSample, LifecycleError> {
    let root = sample
        .payload
        .as_object()
        .ok_or(LifecycleError::InvalidPayload)?;
    let observed_at_ms = sample.observed_at_ms;
    let mut parsed = ParsedSample {
        observed_at_ms,
        drive_timestamp_ms: observed_at_ms,
        charge_timestamp_ms: observed_at_ms,
        climate_timestamp_ms: observed_at_ms,
        vehicle_timestamp_ms: observed_at_ms,
        ..ParsedSample::default()
    };

    let (kind, vehicle_data) = match root.get("record_type").and_then(Value::as_str) {
        Some("owner_api_vehicle_data_v1") => (SourceKind::Full, Some(vehicle_data_object(root)?)),
        Some("fleet_api_vehicle_data_v1") => {
            let feed = root
                .get("provider_feed")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|feed| !feed.is_empty())
                .map(str::to_owned);
            let kind = if feed.is_some() {
                SourceKind::Sparse
            } else {
                SourceKind::Full
            };
            parsed.provider_feed = feed;
            (kind, Some(vehicle_data_object(root)?))
        }
        Some("owner_api_discovery_v1" | "fleet_api_discovery_v1") => (SourceKind::Discovery, None),
        Some("tesla_stream_update_v1") => (
            SourceKind::Stream,
            Some(
                root.get("fields")
                    .and_then(Value::as_object)
                    .ok_or(LifecycleError::InvalidPayload)?,
            ),
        ),
        Some(HUB_EVENT_RECORD_TYPE) => {
            parsed.hub_event = match root.get("event").and_then(Value::as_str) {
                Some("no_data") => Some(HubEventKind::NoData),
                Some("service_refused") => Some(HubEventKind::ServiceRefused),
                _ => None,
            };
            (SourceKind::HubEvent, None)
        }
        _ => (SourceKind::Unknown, None),
    };
    parsed.kind = kind;
    parsed.stream_frame = kind == SourceKind::Stream;
    if matches!(kind, SourceKind::Unknown | SourceKind::HubEvent) {
        return Ok(parsed);
    }

    let drive = group_object(vehicle_data, "drive_state")?;
    let charge = group_object(vehicle_data, "charge_state")?;
    let climate = group_object(vehicle_data, "climate_state")?;
    let vehicle_state = group_object(vehicle_data, "vehicle_state")?;
    let vehicle_config = group_object(vehicle_data, "vehicle_config")?;

    parsed.drive_timestamp_ms = group_time(drive, observed_at_ms);
    parsed.charge_timestamp_ms = group_time(charge, observed_at_ms);
    parsed.climate_timestamp_ms = group_time(climate, observed_at_ms);
    parsed.vehicle_timestamp_ms = group_time(vehicle_state, observed_at_ms);
    parsed.vehicle_state_present = vehicle_state.is_some();
    parsed.drive_data_present = drive.is_some();
    parsed.charge_data_present = charge.is_some();

    if sample.vehicle_state.eq_ignore_ascii_case("online")
        && vehicle_data.is_some()
        && vehicle_state.is_some()
    {
        let raw_car_type = text_field(vehicle_config, "car_type");
        let model = raw_car_type.as_deref().map(normalize_tesla_model_code);
        let trim_badging = text_field(vehicle_config, "trim_badging")
            .map(|value| crate::hub_pack::normalize_tesla_trim(&value));
        let vin = text_field(Some(root), "vin")
            .or_else(|| vehicle_data.and_then(|fields| text_field(Some(fields), "vin")));
        let marketing_name = raw_car_type.as_deref().and_then(|raw| {
            model.as_deref().and_then(|model| {
                crate::hub_pack::derive_tesla_marketing_name(
                    model,
                    trim_badging.as_deref(),
                    Some(raw),
                    vin.as_deref(),
                )
            })
        });
        // A sparse source's `car_version` can be an available, not installed,
        // version (R32), so it never names the installed firmware.
        let firmware_version = if kind == SourceKind::Sparse {
            None
        } else {
            text_field(vehicle_state, "car_version")
        };
        let patch = ProjectionCarPatch {
            name: text_field(Some(root), "display_name")
                .or_else(|| {
                    vehicle_data.and_then(|fields| text_field(Some(fields), "display_name"))
                })
                .or_else(|| text_field(vehicle_state, "vehicle_name")),
            model,
            vin,
            trim_badging,
            marketing_name,
            exterior_color: text_field(vehicle_config, "exterior_color"),
            wheel_type: text_field(vehicle_config, "wheel_type"),
            spoiler_type: text_field(vehicle_config, "spoiler_type"),
            firmware_version,
        };
        parsed.car_metadata = (!patch.is_empty()).then_some(patch);
    }

    // Drive group (R35, R38a, R40).
    parsed.shift_state = text_field(drive, "shift_state");
    parsed.shift_state_key_present =
        drive.is_some_and(|fields| matches!(fields.get("shift_state"), Some(Value::Null)));
    parsed.speed = drive
        .and_then(|fields| float_field(fields, "speed"))
        .map(mph_to_kmh)
        .filter(|speed| (0..=SMALLINT_MAX).contains(speed));
    parsed.power = scaled_field(drive, "power", 0, SMALLINT_MIN as f64, SMALLINT_MAX as f64);
    if let Some((latitude, longitude)) = drive.and_then(|fields| {
        Some((
            round_to_precision(float_field(fields, "latitude")?, 6),
            round_to_precision(float_field(fields, "longitude")?, 6),
        ))
    }) && let Some((latitude, longitude)) = valid_coordinates(Some(latitude), Some(longitude))
    {
        parsed.latitude = Some(latitude);
        parsed.longitude = Some(longitude);
    }
    // `elevation` is the key every source writes; the native value only fills
    // gaps (R38a).
    parsed.elevation =
        ranged_int_field(drive, "elevation", SMALLINT_MIN, SMALLINT_MAX).or_else(|| {
            ranged_int_field(
                drive,
                "native_location_elevation",
                SMALLINT_MIN,
                SMALLINT_MAX,
            )
        });

    // Charge group (R35, R36, R60).
    parsed.charging_state = text_field(charge, "charging_state");
    parsed.battery_level = ranged_int_field(charge, "battery_level", 0, 100);
    parsed.usable_battery_level = ranged_int_field(charge, "usable_battery_level", 0, 100);
    parsed.rated_range_km = miles_field(charge, "battery_range", 2, 0.0, 9_999.99);
    parsed.ideal_range_km = miles_field(charge, "ideal_battery_range", 2, 0.0, 9_999.99);
    parsed.est_range_km = miles_field(charge, "est_battery_range", 2, 0.0, 9_999.99);
    parsed.charge_energy_added = scaled_field(charge, "charge_energy_added", 2, 0.0, 999_999.99);
    let smallint_f64 =
        |key: &str| scaled_field(charge, key, 0, SMALLINT_MIN as f64, SMALLINT_MAX as f64);
    parsed.charger_power_kw = smallint_f64("charger_power");
    parsed.charger_voltage = smallint_f64("charger_voltage");
    parsed.charger_actual_current = smallint_f64("charger_actual_current");
    parsed.charger_pilot_current = smallint_f64("charger_pilot_current");
    // Stored as the car reported it within the smallint column (R36), zero
    // included: a reported zero is a statement, and C2's phase determination
    // alone decides which counts it accepts (C2 R38, R30).
    parsed.charger_phases = ranged_int_field(charge, "charger_phases", SMALLINT_MIN, SMALLINT_MAX);
    parsed.fast_charger_present =
        charge.and_then(|fields| bool_field(fields, "fast_charger_present"));
    parsed.fast_charger_brand = bounded_text_field(charge, "fast_charger_brand");
    parsed.fast_charger_type = bounded_text_field(charge, "fast_charger_type");
    parsed.charge_cable = bounded_text_field(charge, "conn_charge_cable");
    parsed.battery_heater_on = charge.and_then(|fields| bool_field(fields, "battery_heater_on"));
    parsed.not_enough_power_to_heat =
        charge.and_then(|fields| bool_field(fields, "not_enough_power_to_heat"));

    // Climate group (R35).
    let temperature = |key: &str| scaled_field(climate, key, 1, -999.9, 999.9);
    parsed.inside_temp = temperature("inside_temp");
    parsed.outside_temp = temperature("outside_temp");
    parsed.driver_temp_setting = temperature("driver_temp_setting");
    parsed.passenger_temp_setting = temperature("passenger_temp_setting");
    parsed.fan_status = ranged_int_field(
        climate,
        "fan_status",
        i64::from(i32::MIN),
        i64::from(i32::MAX),
    );
    parsed.is_climate_on = climate.and_then(|fields| bool_field(fields, "is_climate_on"));
    parsed.is_rear_defroster_on =
        climate.and_then(|fields| bool_field(fields, "is_rear_defroster_on"));
    parsed.is_front_defroster_on =
        climate.and_then(|fields| bool_field(fields, "is_front_defroster_on"));
    parsed.battery_heater = climate.and_then(|fields| bool_field(fields, "battery_heater"));
    parsed.battery_heater_no_power =
        climate.and_then(|fields| bool_field(fields, "battery_heater_no_power"));

    // Vehicle group (R25, R29, R31, R35).
    parsed.service_mode = vehicle_state.and_then(|fields| bool_field(fields, "service_mode"));
    parsed.car_version = text_field(vehicle_state, "car_version");
    parsed.software_update_status = vehicle_state
        .and_then(|fields| object_field(fields, "software_update"))
        .and_then(|update| update.get("status"))
        .and_then(Value::as_str)
        .map(|status| status.trim().to_owned());
    parsed.odometer = vehicle_state
        .and_then(|fields| float_field(fields, "odometer"))
        .map(miles_to_km)
        .filter(|odometer| *odometer >= 0.0 && odometer.is_finite());
    let pressure = |key: &str| scaled_field(vehicle_state, key, 1, 0.0, 999.9);
    parsed.tpms_pressure_fl = pressure("tpms_pressure_fl");
    parsed.tpms_pressure_fr = pressure("tpms_pressure_fr");
    parsed.tpms_pressure_rl = pressure("tpms_pressure_rl");
    parsed.tpms_pressure_rr = pressure("tpms_pressure_rr");

    // Carried families (R6), judged before any carry-forward.
    parsed.carried = Families {
        motion: parsed.shift_state.is_some()
            || parsed.speed.is_some()
            || parsed.power.is_some()
            || (kind == SourceKind::Full && parsed.shift_state_key_present),
        position: parsed.latitude.is_some() && parsed.longitude.is_some(),
        battery: parsed.battery_level.is_some()
            || parsed.usable_battery_level.is_some()
            || parsed.rated_range_km.is_some()
            || parsed.ideal_range_km.is_some()
            || parsed.est_range_km.is_some(),
        charge_session: parsed.charging_state.is_some()
            || parsed.charge_energy_added.is_some()
            || parsed.charger_power_kw.is_some()
            || parsed.charger_voltage.is_some()
            || parsed.charger_actual_current.is_some()
            || parsed.charger_pilot_current.is_some()
            || parsed.charger_phases.is_some()
            || parsed.fast_charger_present.is_some()
            || parsed.fast_charger_brand.is_some()
            || parsed.fast_charger_type.is_some()
            || parsed.charge_cable.is_some()
            || parsed.battery_heater_on.is_some()
            || parsed.not_enough_power_to_heat.is_some(),
        climate: parsed.is_climate_on.is_some()
            || parsed.inside_temp.is_some()
            || parsed.outside_temp.is_some()
            || parsed.fan_status.is_some()
            || parsed.driver_temp_setting.is_some()
            || parsed.passenger_temp_setting.is_some()
            || parsed.is_rear_defroster_on.is_some()
            || parsed.is_front_defroster_on.is_some()
            || parsed.battery_heater.is_some()
            || parsed.battery_heater_no_power.is_some(),
        odometer: parsed.odometer.is_some(),
        vehicle_status: parsed.service_mode.is_some()
            || parsed.car_version.is_some()
            || parsed.software_update_status.is_some()
            || parsed.tpms_pressure_fl.is_some()
            || parsed.tpms_pressure_fr.is_some()
            || parsed.tpms_pressure_rl.is_some()
            || parsed.tpms_pressure_rr.is_some(),
    };
    Ok(parsed)
}

/// Trimmed text of a JSON string field; empty is `None` (C1 R4a).
fn text_field(fields: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    fields
        .and_then(|fields| fields.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// A timestamp field (C1 R5): a positive integer, integral float or numeric
/// string. Range checks against the observation time are in [`group_time`].
pub(crate) fn timestamp_field(fields: &Map<String, Value>, key: &str) -> Option<i64> {
    let value = fields.get(key)?;
    let timestamp = value
        .as_i64()
        .or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().parse::<i64>().ok())
        })
        .or_else(|| {
            let number = value.as_f64()?;
            (number.is_finite()
                && number >= 1.0
                && number <= i64::MAX as f64
                && number.fract() == 0.0)
                .then_some(number as i64)
        })?;
    (timestamp > 0).then_some(timestamp)
}

#[cfg(test)]
mod stream_fixture_tests {
    use super::*;
    use crate::tesla_stream::parse_data_update;

    fn sample(id: i64, timestamp_ms: i64, odometer: f64, shift_state: &str) -> LifecycleSample {
        let frame = format!(
            r#"{{"msg_type":"data:update","tag":"9","value":"{timestamp_ms},25,51.5,-0.1,210,180,{odometer},120,200,{shift_state},80,{speed}"}}"#,
            speed = if shift_state == "P" { 0 } else { 10 },
        );
        let update = parse_data_update(&frame).unwrap();
        LifecycleSample {
            observation_id: id,
            observed_at_ms: timestamp_ms,
            vehicle_state: "online".to_owned(),
            payload: stream_observation_payload(&update),
        }
    }

    fn owner_park(id: i64, timestamp_ms: i64, odometer_miles: f64) -> LifecycleSample {
        LifecycleSample {
            observation_id: id,
            observed_at_ms: timestamp_ms,
            vehicle_state: "online".to_owned(),
            payload: json!({
                "record_type": "owner_api_vehicle_data_v1",
                "vehicle_data": {
                    "drive_state": {
                        "shift_state": "P",
                        "speed": 0,
                        "latitude": 51.5,
                        "longitude": -0.1,
                        "timestamp": timestamp_ms
                    },
                    "vehicle_state": {
                        "odometer": odometer_miles,
                        "timestamp": timestamp_ms
                    }
                }
            }),
        }
    }

    #[test]
    fn stream_drive_closes_before_owner_poll_and_duplicate_is_idempotent() {
        let state = OpenSessionState::new();
        let parked = sample(1, 1_700_000_000_000, 100.0, "P");
        let driving_one = sample(2, 1_700_000_001_000, 100.0, "D");
        let driving_two = sample(3, 1_700_000_002_000, 100.2, "D");
        let parked_again = owner_park(4, 1_700_000_003_000, 100.3);

        let first = apply_sample(state, 9, &parked).unwrap();
        let second = apply_sample(first.state, 9, &driving_one).unwrap();
        let third = apply_sample(second.state, 9, &driving_two).unwrap();
        let closed = apply_sample(third.state, 9, &parked_again).unwrap();
        assert_eq!(closed.delta.drives.len(), 1);
        // R11: the children of an open drive travel as open positions; the
        // closing observation's position stays with the drive it closes.
        assert_eq!(
            closed
                .delta
                .open_drive_positions
                .iter()
                .filter(|position| position.drive_id == Some(1))
                .count(),
            1
        );
        assert_eq!(
            second
                .delta
                .open_drive_positions
                .iter()
                .chain(third.delta.open_drive_positions.iter())
                .filter(|position| position.drive_id == Some(1))
                .count(),
            2
        );
        assert_eq!(
            closed.delta.drives[0].start_date_ms,
            driving_one.observed_at_ms
        );
        assert_eq!(
            closed.delta.drives[0].end_date_ms,
            parked_again.observed_at_ms
        );
        // R41a: the parked frame is within `T_gap`, so the distance counts the
        // lead from the parked spot: 100.3 - 100.0 miles.
        let distance = closed.delta.drives[0].distance_km.unwrap();
        assert!((distance - 0.3 * K_MILE_KM).abs() < 1e-9, "{distance}");

        let duplicate = apply_sample(closed.state, 9, &parked_again).unwrap();
        assert!(duplicate.delta.drives.is_empty());
        assert!(duplicate.delta.positions.is_empty());
    }

    fn stream_sample(id: i64, timestamp_ms: i64, speed: i64, shift_state: &str) -> LifecycleSample {
        let frame = format!(
            r#"{{"msg_type":"data:update","tag":"9","value":"{timestamp_ms},25,51.5,-0.1,210,180,100.0,120,200,{shift_state},80,{speed}"}}"#,
        );
        let update = parse_data_update(&frame).unwrap();
        LifecycleSample {
            observation_id: id,
            observed_at_ms: timestamp_ms,
            vehicle_state: "online".to_owned(),
            payload: stream_observation_payload(&update),
        }
    }

    #[test]
    fn numeric_speed_without_drive_shift_does_not_open_a_drive() {
        let parked_with_speed = stream_sample(1, 1_700_000_000_000, 25, "P");
        let empty_shift_with_speed = stream_sample(2, 1_700_000_001_000, 25, "");
        let driving = stream_sample(3, 1_700_000_002_000, 0, "D");
        let reverse = stream_sample(4, 1_700_000_003_000, 0, "R");
        let neutral = stream_sample(5, 1_700_000_004_000, 0, "N");

        let parked = apply_sample(OpenSessionState::new(), 9, &parked_with_speed).unwrap();
        assert!(parked.state.open_drive.is_none());
        assert_eq!(parked.state.phase, VehiclePhase::Online);

        // R40: no gear and a measured speed is Motion, so this opens a drive.
        let empty = apply_sample(parked.state, 9, &empty_shift_with_speed).unwrap();
        assert!(empty.state.open_drive.is_some());
        assert_eq!(empty.state.phase, VehiclePhase::Driving);

        let in_drive = apply_sample(OpenSessionState::new(), 9, &driving).unwrap();
        assert!(in_drive.state.open_drive.is_some());
        assert_eq!(in_drive.state.phase, VehiclePhase::Driving);

        let in_reverse = apply_sample(OpenSessionState::new(), 9, &reverse).unwrap();
        assert!(in_reverse.state.open_drive.is_some());
        let in_neutral = apply_sample(OpenSessionState::new(), 9, &neutral).unwrap();
        assert!(in_neutral.state.open_drive.is_some());
    }

    #[test]
    fn newer_local_id_with_older_provider_time_only_advances_cursor() {
        let current = sample(1, 1_700_000_002_000, 100.2, "P");
        let projected = apply_sample(OpenSessionState::new(), 9, &current).unwrap();
        let expected_phase = projected.state.phase;
        let expected_timestamp = projected.state.last_observed_at_ms;

        let stale = sample(2, 1_700_000_001_000, 99.9, "D");
        let ignored = apply_sample(projected.state, 9, &stale).unwrap();

        assert_eq!(ignored.state.last_observation_id, 2);
        assert_eq!(ignored.state.last_observed_at_ms, expected_timestamp);
        assert_eq!(ignored.state.phase, expected_phase);
        assert_eq!(
            ignored.state.last_drive_timestamp_ms,
            Some(current.observed_at_ms)
        );
        assert_eq!(ignored.delta, LifecycleDelta::default());
    }
}
