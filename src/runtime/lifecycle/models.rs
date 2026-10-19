// SPDX-License-Identifier: AGPL-3.0-only

/// One ordered observation already validated and stored by the Hub.
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleSample {
    pub observation_id: i64,
    pub observed_at_ms: i64,
    pub vehicle_state: String,
    pub payload: Value,
}

/// Durable open-session state. Crash recovery reloads this exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OpenSessionState {
    pub version: u32,
    pub last_observation_id: i64,
    /// Durable proof that imported/materialised entity maxima have already
    /// seeded the live ID cursors. Older state blobs decode as `false` and do
    /// one indexed seed pass on their next collection.
    #[serde(default)]
    pub id_cursors_seeded: bool,
    #[serde(default)]
    pub last_observed_at_ms: Option<i64>,
    #[serde(default)]
    pub last_drive_timestamp_ms: Option<i64>,
    #[serde(default)]
    pub last_charge_timestamp_ms: Option<i64>,
    #[serde(default)]
    pub last_vehicle_timestamp_ms: Option<i64>,
    #[serde(default)]
    pub imported_drive_watermark_ms: Option<i64>,
    #[serde(default)]
    pub imported_charge_watermark_ms: Option<i64>,
    #[serde(default)]
    pub imported_state_watermark_ms: Option<i64>,
    pub next_drive_id: i64,
    pub next_position_id: i64,
    pub next_charge_id: i64,
    pub next_charge_sample_id: i64,
    #[serde(default = "default_next_state_id")]
    pub next_state_id: i64,
    #[serde(default = "default_next_update_id")]
    pub next_update_id: i64,
    #[serde(default)]
    pub last_stationary_position_at_ms: Option<i64>,
    pub phase: VehiclePhase,
    pub open_drive: Option<OpenDrive>,
    /// Persisted member kept so existing state blobs still decode; the C1
    /// replacement defines whether and how it is used.
    #[serde(default)]
    pub pending_gained_range_charge: Option<GainedRangeChargeSeed>,
    pub open_charge: Option<OpenCharge>,
    #[serde(default)]
    pub open_state: Option<OpenState>,
    #[serde(default)]
    pub open_update: Option<OpenUpdate>,
    #[serde(default)]
    pub imported_open: Option<ImportedOpenSessionRefs>,
    #[serde(default)]
    pub service_mode: Option<bool>,
    #[serde(default)]
    pub car_metadata: Option<ProjectionCarPatch>,
    #[serde(default)]
    pub last_position_battery_heater: Option<bool>,
    #[serde(default)]
    pub last_position_battery_heater_on: Option<bool>,
    #[serde(default)]
    pub last_position_battery_heater_no_power: Option<bool>,
    #[serde(default)]
    pub last_position_est_battery_range_km: Option<f64>,
    #[serde(default)]
    pub last_position_fan_status: Option<i64>,
    #[serde(default)]
    pub last_position_driver_temp_setting: Option<f64>,
    #[serde(default)]
    pub last_position_passenger_temp_setting: Option<f64>,
    #[serde(default)]
    pub last_position_is_rear_defroster_on: Option<bool>,
    #[serde(default)]
    pub last_position_is_front_defroster_on: Option<bool>,
    #[serde(default)]
    pub last_position_tpms_pressure_fl: Option<f64>,
    #[serde(default)]
    pub last_position_tpms_pressure_fr: Option<f64>,
    #[serde(default)]
    pub last_position_tpms_pressure_rl: Option<f64>,
    #[serde(default)]
    pub last_position_tpms_pressure_rr: Option<f64>,
    /// Last known values of the parked car (C1 R69). Needed by the energy
    /// baseline (R61a) and the after-gap charge (R70); cleared when a drive
    /// opens (R41).
    #[serde(default)]
    pub parked_snapshot: Option<ParkedSnapshot>,
    /// Newest comparable installed version text seen on a full source (R31).
    #[serde(default)]
    pub last_car_version: Option<String>,
    /// Vehicle-group time at which `last_car_version` was last seen (R31):
    /// the lower bound of the start of an update missed while unobserved.
    #[serde(default)]
    pub last_car_version_at_ms: Option<i64>,
    /// Coordinates of the last standalone or drive-end position (R53), the
    /// reference for the "moved" trigger of standalone positions.
    #[serde(default)]
    pub last_standalone_latitude: Option<f64>,
    #[serde(default)]
    pub last_standalone_longitude: Option<f64>,
    /// Last accepted reachability, `online`, `asleep` or `offline` (R7). It is
    /// the reference of discovery delivery (R77) and of the phase (R21).
    #[serde(default)]
    pub last_reach: Option<String>,
    /// Watermarks of the position, battery, climate and odometer fact
    /// families (R6). The other families keep the retained group watermarks.
    #[serde(default)]
    pub last_position_fact_ms: Option<i64>,
    #[serde(default)]
    pub last_battery_fact_ms: Option<i64>,
    #[serde(default)]
    pub last_climate_fact_ms: Option<i64>,
    #[serde(default)]
    pub last_odometer_fact_ms: Option<i64>,
    /// `service_mode` was set by a `service_refused` Hub event and not by a
    /// vehicle statement, so any later full-source answer can clear it (R25).
    #[serde(default)]
    pub service_mode_refused: bool,
}

/// Last known values of the parked car before a gap (C1 R69).
///
/// All fields are plain facts of the car at `seen_at_ms`; `at_ms` is the latest
/// time up to which the charged energy is accounted for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ParkedSnapshot {
    #[serde(default)]
    pub at_ms: Option<i64>,
    #[serde(default)]
    pub seen_at_ms: i64,
    #[serde(default)]
    pub battery_level: Option<i64>,
    #[serde(default)]
    pub ideal_range_km: Option<f64>,
    #[serde(default)]
    pub rated_range_km: Option<f64>,
    #[serde(default)]
    pub odometer_km: Option<f64>,
    #[serde(default)]
    pub odometer_coarse: bool,
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    #[serde(default)]
    pub energy_added_kwh: Option<f64>,
    #[serde(default)]
    pub after_drive: bool,
    #[serde(default)]
    pub unplugged: bool,
}

/// Bounded reference to normalized imported open-session rows. Child rows stay
/// in SQLite; this record never grows with telemetry volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ImportedOpenSessionRefs {
    pub source_id: String,
    #[serde(default)]
    pub content_sha256: String,
    pub drive_source_row_id: Option<i64>,
    #[serde(default)]
    pub drive_position_count: u64,
    pub charge_source_row_id: Option<i64>,
    #[serde(default)]
    pub charge_sample_count: u64,
    pub state_source_row_id: Option<i64>,
    pub standalone_position_count: u64,
}

impl OpenSessionState {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            next_drive_id: 1,
            next_position_id: 1,
            next_charge_id: 1,
            next_charge_sample_id: 1,
            next_state_id: 1,
            next_update_id: 1,
            ..Self::default()
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, LifecycleError> {
        let bytes = serde_json::to_vec(self).map_err(|_| LifecycleError::SessionEncode)?;
        if bytes.len() > MAX_OPEN_SESSION_BYTES {
            return Err(LifecycleError::SessionTooLarge {
                actual: bytes.len(),
                maximum: MAX_OPEN_SESSION_BYTES,
            });
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, LifecycleError> {
        if bytes.len() > MAX_OPEN_SESSION_BYTES {
            return Err(LifecycleError::SessionTooLarge {
                actual: bytes.len(),
                maximum: MAX_OPEN_SESSION_BYTES,
            });
        }
        let state: Self =
            serde_json::from_slice(bytes).map_err(|_| LifecycleError::SessionDecode)?;
        if state.version != Self::CURRENT_VERSION {
            return Err(LifecycleError::UnsupportedSessionVersion(state.version));
        }
        if state.next_drive_id < 1
            || state.next_position_id < 1
            || state.next_charge_id < 1
            || state.next_charge_sample_id < 1
            || state.next_state_id < 1
            || state.next_update_id < 1
            || state.last_observation_id < 0
        {
            return Err(LifecycleError::CorruptSession);
        }
        Ok(state)
    }
}

/// The largest id already in use in each id space of one vehicle (R84): the
/// larger of the materialised maximum and the import watermark of the matching
/// `lifecycle_source_watermarks` domain. Zero means none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct IdFloors {
    pub drives: i64,
    pub positions: i64,
    pub charges: i64,
    pub charge_samples: i64,
    pub states: i64,
    pub updates: i64,
}

impl OpenSessionState {
    /// Raise every `next_*_id` to at least `floor + 1`, saturating at
    /// `i64::MAX`, and never lower one (R84). The next allocation is then the
    /// first id above every key in use. Ids already allocated are not
    /// renumbered. Reason: an id at or below an imported key would alias two
    /// rows on the pages that serve imported rows, and would make a live
    /// charge look imported (C2 R5); a cursor that is too high only skips
    /// unused values.
    pub(crate) fn raise_id_cursors(&mut self, used: &IdFloors) {
        let raise =
            |next: &mut i64, floor: i64| *next = (*next).max(floor.max(0).saturating_add(1));
        raise(&mut self.next_drive_id, used.drives);
        raise(&mut self.next_position_id, used.positions);
        raise(&mut self.next_charge_id, used.charges);
        raise(&mut self.next_charge_sample_id, used.charge_samples);
        raise(&mut self.next_state_id, used.states);
        raise(&mut self.next_update_id, used.updates);
    }
}

/// Seed only the open parents and bounded counters from an imported snapshot.
/// The normalized child rows are loaded by the store when a continuation view
/// is requested, rather than being copied into the 64 KiB lifecycle blob.
pub fn seed_imported_open_session_state(
    source_id: uuid::Uuid,
    session: &TeslaMateOpenSession,
    existing: Option<&OpenSessionState>,
) -> Result<OpenSessionState, LifecycleError> {
    session
        .validate()
        .map_err(|_| LifecycleError::InvalidImportedSession)?;
    let mut state = existing.cloned().unwrap_or_else(OpenSessionState::new);
    let refs = ImportedOpenSessionRefs {
        source_id: source_id.to_string(),
        content_sha256: imported_open_content_sha256(session)?,
        drive_source_row_id: session.drive.as_ref().map(|row| row.id),
        drive_position_count: session.drive_positions.len() as u64,
        charge_source_row_id: session.charge.as_ref().map(|row| row.id),
        charge_sample_count: session.charge_samples.len() as u64,
        state_source_row_id: session.state.as_ref().map(|row| row.id),
        standalone_position_count: session.standalone_positions.len() as u64,
    };
    let same_open_refs = state.imported_open.as_ref() == Some(&refs);
    state.imported_drive_watermark_ms = state.imported_drive_watermark_ms.max(
        session
            .watermarks
            .drives
            .max_timestamp_ms
            .max(session.watermarks.positions.max_timestamp_ms),
    );
    state.imported_charge_watermark_ms = state.imported_charge_watermark_ms.max(
        session
            .watermarks
            .charging_processes
            .max_timestamp_ms
            .max(session.watermarks.charges.max_timestamp_ms),
    );
    state.imported_state_watermark_ms = state
        .imported_state_watermark_ms
        .max(session.watermarks.states.max_timestamp_ms);
    let max_drive = session
        .watermarks
        .drives
        .max_id
        .unwrap_or(0)
        .max(session.drive.as_ref().map_or(0, |row| row.id));
    let max_position = session
        .drive_positions
        .iter()
        .chain(session.standalone_positions.iter())
        .map(|row| row.id)
        .max()
        .unwrap_or(0)
        .max(session.watermarks.positions.max_id.unwrap_or(0));
    let max_charge = session
        .watermarks
        .charging_processes
        .max_id
        .unwrap_or(0)
        .max(session.charge.as_ref().map_or(0, |row| row.id));
    let max_sample = session
        .charge_samples
        .iter()
        .map(|row| row.id)
        .max()
        .unwrap_or(0)
        .max(session.watermarks.charges.max_id.unwrap_or(0));
    let max_state = session
        .watermarks
        .states
        .max_id
        .unwrap_or(0)
        .max(session.state.as_ref().map_or(0, |row| row.id));
    state.next_drive_id = state.next_drive_id.max(max_drive.saturating_add(1));
    state.next_position_id = state.next_position_id.max(max_position.saturating_add(1));
    state.next_charge_id = state.next_charge_id.max(max_charge.saturating_add(1));
    state.next_charge_sample_id = state
        .next_charge_sample_id
        .max(max_sample.saturating_add(1));
    state.next_state_id = state.next_state_id.max(max_state.saturating_add(1));
    state.next_update_id = state.next_update_id.max(
        session
            .watermarks
            .updates
            .max_id
            .unwrap_or(0)
            .saturating_add(1),
    );
    state.id_cursors_seeded = true;
    state.last_observed_at_ms = state
        .last_observed_at_ms
        .max(session_max_timestamp(session));
    if same_open_refs {
        return Ok(state);
    }
    state.imported_open = Some(refs);
    state.open_drive = session
        .drive
        .as_ref()
        .map(|row| open_drive_from_source(row, &session.drive_positions));
    state.open_charge = session
        .charge
        .as_ref()
        .map(|row| open_charge_from_source(row, &session.charge_samples));
    state.open_state = session.state.as_ref().map(open_state_from_source);
    state.phase = if state.open_drive.is_some() {
        VehiclePhase::Driving
    } else if state.open_charge.is_some() {
        VehiclePhase::Charging
    } else if let Some(open_state) = state.open_state.as_ref() {
        phase_from_vehicle_state(&open_state.state)
    } else {
        VehiclePhase::Online
    };
    Ok(state)
}

fn imported_open_content_sha256(session: &TeslaMateOpenSession) -> Result<String, LifecycleError> {
    struct DigestWriter(Sha256);
    impl std::io::Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(
        &mut writer,
        &(
            &session.drive,
            &session.drive_positions,
            &session.charge,
            &session.charge_samples,
            &session.state,
        ),
    )
    .map_err(|_| LifecycleError::InvalidImportedSession)?;
    Ok(hex::encode(writer.0.finalize()))
}

fn session_max_timestamp(session: &TeslaMateOpenSession) -> Option<i64> {
    session
        .drive_positions
        .iter()
        .map(|row| row.date_ms)
        .chain(session.standalone_positions.iter().map(|row| row.date_ms))
        .chain(session.charge_samples.iter().map(|row| row.date_ms))
        .chain(session.drive.iter().map(|row| row.start_date_ms))
        .chain(session.charge.iter().map(|row| row.start_date_ms))
        .chain(session.state.iter().map(|row| row.start_date_ms))
        .max()
}

fn open_drive_from_source(row: &TeslaMateDrive, positions: &[TeslaMatePosition]) -> OpenDrive {
    let mut open = OpenDrive {
        id: row.id,
        car_id: row.car_id,
        start_date_ms: row.start_date_ms,
        start_latitude: None,
        start_longitude: None,
        start_soc: None,
        start_rated_range_km: row.start_rated_range_km,
        speed_max: row.speed_max,
        outside_temp_sum: 0.0,
        outside_temp_count: 0,
        position_count: 0,
        last_position_date_ms: None,
        last_latitude: None,
        last_longitude: None,
        last_soc: None,
        last_rated_range_km: None,
        last_odometer: None,
        first_odometer: None,
        power_max: row.power_max,
        power_min: row.power_min,
        inside_temp_sum: 0.0,
        inside_temp_count: 0,
        start_ideal_range_km: row.start_ideal_range_km,
        end_ideal_range_km: row.end_ideal_range_km,
        elevation_ascent: 0,
        elevation_descent: 0,
        last_elevation: None,
        saw_offline: false,
        last_charge_energy_added: None,
        last_ideal_range_km: row.end_ideal_range_km.or(row.start_ideal_range_km),
        last_folded_at_ms: None,
        last_motion_at_ms: None,
        max_displacement_m: 0.0,
        elevation_count: 0,
        odometer_coarse: false,
        still_checkpoint: None,
        lead_checkpoints: Vec::new(),
        positions: Vec::new(),
    };

    let mut ordered = positions.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|position| (position.date_ms, position.id));
    if let Some(first) = ordered.first() {
        open.start_latitude = Some(first.latitude);
        open.start_longitude = Some(first.longitude);
        open.start_soc = first.battery_level;
        open.start_rated_range_km = open.start_rated_range_km.or(first.rated_battery_range_km);
    }
    for position in ordered {
        let position = imported_position(position);
        if let Some(speed) = position.speed {
            open.speed_max = Some(open.speed_max.map_or(speed, |max| max.max(speed)));
        }
        observe_drive_position(&mut open, &position);
    }
    open.first_odometer = row.start_km.or(open.first_odometer);
    open.last_odometer = row.end_km.or(open.last_odometer);
    open.elevation_ascent = row.ascent.unwrap_or(open.elevation_ascent);
    open.elevation_descent = row.descent.unwrap_or(open.elevation_descent);
    if let Some(average) = row.outside_temp_avg {
        let count = open.outside_temp_count.max(1);
        open.outside_temp_sum = average * f64::from(count);
        open.outside_temp_count = count;
    }
    if let Some(average) = row.inside_temp_avg {
        let count = open.inside_temp_count.max(1);
        open.inside_temp_sum = average * f64::from(count);
        open.inside_temp_count = count;
    }
    open
}

fn open_charge_from_source(
    row: &TeslaMateChargingProcess,
    samples: &[TeslaMateCharge],
) -> OpenCharge {
    let mut open = OpenCharge {
        id: row.id,
        car_id: row.car_id,
        start_date_ms: row.start_date_ms,
        start_battery_level: row.start_battery_level,
        start_ideal_range_km: row.start_ideal_range_km,
        start_rated_range_km: row.start_rated_range_km,
        start_latitude: None,
        start_longitude: None,
        is_dc: None,
        fast_charger_type: None,
        max_charger_power_kw: None,
        outside_temp_sum: 0.0,
        outside_temp_count: 0,
        first_energy_added: None,
        max_energy_added: None,
        last_energy_added: None,
        last_battery_level: row.end_battery_level.or(row.start_battery_level),
        last_ideal_range_km: row.end_ideal_range_km.or(row.start_ideal_range_km),
        last_rated_range_km: row.end_rated_range_km.or(row.start_rated_range_km),
        sample_count: 0,
        energy_used_kwh: None,
        last_sample_timestamp_ms: None,
        last_sample_power_kw: None,
        energy_added_carry_kwh: 0.0,
        prev_energy_added_kwh: None,
        ratio_used_kwh: 0.0,
        ratio_added_kwh: 0.0,
        fully_powered: false,
        pre_sample_kwh: None,
        phase_count: None,
        last_odometer_km: None,
        last_odometer_coarse: None,
        samples: Vec::new(),
    };

    let mut ordered = samples.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|sample| (sample.date_ms, sample.id));
    for sample in ordered {
        let sample = project_charge_sample(sample);
        if open.sample_count == 0 {
            open.start_battery_level = open.start_battery_level.or(sample.battery_level);
            open.start_ideal_range_km = open.start_ideal_range_km.or(sample.ideal_range_km);
            open.start_rated_range_km = open.start_rated_range_km.or(sample.rated_range_km);
        }
        open.last_battery_level = sample.battery_level.or(open.last_battery_level);
        open.last_ideal_range_km = sample.ideal_range_km.or(open.last_ideal_range_km);
        open.last_rated_range_km = sample.rated_range_km.or(open.last_rated_range_km);
        if let Some(power) = sample.charger_power_kw {
            open.max_charger_power_kw = Some(
                open.max_charger_power_kw
                    .map_or(power, |max| max.max(power)),
            );
        }
        // `is_dc` and `fast_charger_type` are written only by
        // `observe_charge_sample` (C2 R9 S2, R31, R32), called below.
        if let Some(temp) = sample.outside_temp_c {
            open.outside_temp_sum += temp;
            open.outside_temp_count = open.outside_temp_count.saturating_add(1);
        }
        observe_charge_sample(&mut open, &sample);
    }
    // The energy fields are only the fold of the imported samples above (C2
    // R14): the imported process row's own energy columns are never read, so a
    // seeded session and a live one follow one rule.
    if let Some(average) = row.outside_temp_avg {
        let count = open.outside_temp_count.max(1);
        open.outside_temp_sum = average * f64::from(count);
        open.outside_temp_count = count;
    }
    open
}

fn open_state_from_source(row: &TeslaMateState) -> OpenState {
    OpenState {
        id: row.id,
        car_id: row.car_id,
        state: row.state.clone(),
        start_date_ms: row.start_date_ms,
    }
}

pub fn imported_position(row: &TeslaMatePosition) -> ProjectionPosition {
    ProjectionPosition {
        id: row.id,
        drive_id: row.drive_id,
        car_id: row.car_id,
        date_ms: row.date_ms,
        latitude: row.latitude,
        longitude: row.longitude,
        speed: row.speed,
        power: row.power,
        battery_level: row.battery_level,
        usable_battery_level: row.usable_battery_level,
        elevation: row.elevation,
        odometer: row.odometer,
        ideal_battery_range_km: row.ideal_battery_range_km,
        est_battery_range_km: row.est_battery_range_km,
        rated_battery_range_km: row.rated_battery_range_km,
        fan_status: row.fan_status,
        driver_temp_setting: row.driver_temp_setting,
        passenger_temp_setting: row.passenger_temp_setting,
        is_climate_on: row.is_climate_on,
        is_rear_defroster_on: row.is_rear_defroster_on,
        is_front_defroster_on: row.is_front_defroster_on,
        inside_temp: row.inside_temp,
        outside_temp: row.outside_temp,
        battery_heater: row.battery_heater,
        battery_heater_on: row.battery_heater_on,
        battery_heater_no_power: row.battery_heater_no_power,
        tpms_pressure_fl: row.tpms_pressure_fl,
        tpms_pressure_fr: row.tpms_pressure_fr,
        tpms_pressure_rl: row.tpms_pressure_rl,
        tpms_pressure_rr: row.tpms_pressure_rr,
    }
}

/// Phase of the vehicle (C1 R20). `Suspended` is never produced; it stays in
/// the enum so stored blobs that contain it decode, and is read as `Online`
/// through [`VehiclePhase::effective`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VehiclePhase {
    #[default]
    Unknown,
    Online,
    Asleep,
    Offline,
    Suspended,
    Driving,
    Charging,
    Updating,
}

impl VehiclePhase {
    /// The phase as the unit reads it: `Suspended` is the logical state
    /// `online` (R20, R23), every other phase is itself.
    pub fn effective(self) -> Self {
        match self {
            Self::Suspended => Self::Online,
            other => other,
        }
    }
}

impl OpenDrive {
    /// Time of the latest folded observation; old blobs fall back to the last
    /// stored position, then the start (C1 R12).
    pub(crate) fn effective_last_folded_ms(&self) -> i64 {
        self.last_folded_at_ms
            .or(self.last_position_date_ms)
            .unwrap_or(self.start_date_ms)
    }

    /// Time of the latest Motion fold; same fallback as the fold time (R12).
    pub(crate) fn effective_last_motion_ms(&self) -> i64 {
        self.last_motion_at_ms
            .or(self.last_position_date_ms)
            .unwrap_or(self.start_date_ms)
    }

    /// Number of elevations in the walk; an old blob with a reference but no
    /// count has seen one value (R12).
    pub(crate) fn effective_elevation_count(&self) -> u32 {
        if self.elevation_count == 0 && self.last_elevation.is_some() {
            1
        } else {
            self.elevation_count
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenDrive {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
    pub start_latitude: Option<f64>,
    pub start_longitude: Option<f64>,
    pub start_soc: Option<i64>,
    pub start_rated_range_km: Option<f64>,
    pub speed_max: Option<i64>,
    pub outside_temp_sum: f64,
    pub outside_temp_count: u32,
    /// Running child count. Children live in `lifecycle_open_rows`; this state
    /// must not grow with telemetry volume.
    #[serde(default)]
    pub position_count: u32,
    #[serde(default)]
    pub last_position_date_ms: Option<i64>,
    #[serde(default)]
    pub last_latitude: Option<f64>,
    #[serde(default)]
    pub last_longitude: Option<f64>,
    #[serde(default)]
    pub last_soc: Option<i64>,
    #[serde(default)]
    pub last_rated_range_km: Option<f64>,
    #[serde(default)]
    pub last_odometer: Option<f64>,
    #[serde(default)]
    pub first_odometer: Option<f64>,
    #[serde(default)]
    pub power_max: Option<f64>,
    #[serde(default)]
    pub power_min: Option<f64>,
    #[serde(default)]
    pub inside_temp_sum: f64,
    #[serde(default)]
    pub inside_temp_count: u32,
    #[serde(default)]
    pub start_ideal_range_km: Option<f64>,
    #[serde(default)]
    pub end_ideal_range_km: Option<f64>,
    #[serde(default)]
    pub elevation_ascent: i64,
    #[serde(default)]
    pub elevation_descent: i64,
    #[serde(default)]
    pub last_elevation: Option<i64>,
    /// Persisted member kept so existing state blobs still decode; the C1
    /// replacement defines whether and how it is used.
    #[serde(default)]
    pub saw_offline: bool,
    /// Persisted member kept so existing state blobs still decode; the C1
    /// replacement defines whether and how it is used.
    #[serde(default)]
    pub last_charge_energy_added: Option<f64>,
    /// Persisted member kept so existing state blobs still decode; the C1
    /// replacement defines whether and how it is used.
    #[serde(default)]
    pub last_ideal_range_km: Option<f64>,
    /// Time of the latest folded observation (R42). Absent in old blobs: read
    /// `last_position_date_ms`, else the start (R12).
    #[serde(default)]
    pub last_folded_at_ms: Option<i64>,
    /// Time of the latest Motion fold (R42, R44). Same fallback as above.
    #[serde(default)]
    pub last_motion_at_ms: Option<i64>,
    /// Greatest great-circle distance from the start coordinates, in metres (R47).
    #[serde(default)]
    pub max_displacement_m: f64,
    /// Number of valid elevations stepped into the walk (R51). Absent in old
    /// blobs: 1 when `last_elevation` is set, else 0.
    #[serde(default)]
    pub elevation_count: u32,
    /// An odometer folded into this drive came from a stream or sparse source
    /// and may be too coarse to decide the keep rule (R47).
    #[serde(default)]
    pub odometer_coarse: bool,
    /// Copy of the drive (without positions and without its own checkpoint)
    /// taken at the first still fold after the last motion (R44).
    #[serde(default)]
    pub still_checkpoint: Option<Box<OpenDrive>>,
    /// Copies of the drive taken immediately before each lead fold (a fold of a
    /// fresh position with no fresh motion family, dated after the motion
    /// watermark), ordered by `cut_ms` ascending and at most
    /// [`K_LEAD_CHECKPOINTS`] long (C1 R87). They let a Park or plugged-in fact
    /// dated before such folds end the drive at its own time. Absent in blobs
    /// from before R87, which decode with none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lead_checkpoints: Vec<LeadCheckpoint>,
    /// In-memory child buffer for pure unit tests and single-batch close. The
    /// durable db path clears this before encoding and never rehydrates the full
    /// history into active state on every observation.
    #[serde(default)]
    pub positions: Vec<ProjectionPosition>,
}

/// One lead checkpoint of an open drive (C1 R87): the drive as it stood just
/// before a lead fold dated `cut_ms`, without positions, still checkpoint or
/// lead checkpoints of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeadCheckpoint {
    pub cut_ms: i64,
    pub drive: OpenDrive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenCharge {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
    pub start_battery_level: Option<i64>,
    #[serde(default)]
    pub start_ideal_range_km: Option<f64>,
    pub start_rated_range_km: Option<f64>,
    #[serde(default)]
    pub start_latitude: Option<f64>,
    #[serde(default)]
    pub start_longitude: Option<f64>,
    pub is_dc: Option<bool>,
    #[serde(default)]
    pub fast_charger_type: Option<String>,
    pub max_charger_power_kw: Option<f64>,
    pub outside_temp_sum: f64,
    pub outside_temp_count: u32,
    #[serde(default)]
    pub first_energy_added: Option<f64>,
    #[serde(default)]
    pub max_energy_added: Option<f64>,
    pub last_energy_added: Option<f64>,
    pub last_battery_level: Option<i64>,
    #[serde(default)]
    pub last_ideal_range_km: Option<f64>,
    pub last_rated_range_km: Option<f64>,
    #[serde(default)]
    pub sample_count: u32,
    /// Incremental energy-used accumulator so close does not need every sample.
    #[serde(default)]
    pub energy_used_kwh: Option<f64>,
    #[serde(default)]
    pub last_sample_timestamp_ms: Option<i64>,
    #[serde(default)]
    pub last_sample_power_kw: Option<f64>,
    /// Energy of the finished counter segments, in kWh: `k` of C2 R11. Each
    /// counter restart (`counter_restarted`, C2 R76: a fall beyond the dip
    /// tolerance) adds the segment that ended. Zero in every live session,
    /// where C1 R66 closes the session at a restart before the fold sees it; a
    /// dip changes nothing here.
    #[serde(default)]
    pub energy_added_carry_kwh: f64,
    /// The previous sample's usable counter, or `None` when it had none (C2
    /// R18). Decides whether an interval is counted for the ratio sums.
    #[serde(default)]
    pub prev_energy_added_kwh: Option<f64>,
    /// `U_r`: inlet energy over the powered, counted intervals, in kWh (C2
    /// R18).
    #[serde(default)]
    pub ratio_used_kwh: f64,
    /// `A_r`: counter energy over the same intervals, in kWh (C2 R18).
    #[serde(default)]
    pub ratio_added_kwh: f64,
    /// Every interval of the session so far was powered (C2 R18). Absent in
    /// old blobs, which decode as `false`, so an upgraded session never claims
    /// a gap-free integral it did not measure (C2 R25).
    #[serde(default)]
    pub fully_powered: bool,
    /// Battery energy that arrived before the first sample, `c - b` of the
    /// first sample, in kWh (C2 R18, R19 row 3). `None` when the first sample
    /// had no usable counter.
    #[serde(default)]
    pub pre_sample_kwh: Option<f64>,
    /// The latest phase count a sample itself fixed (C2 R36).
    #[serde(default)]
    pub phase_count: Option<i64>,
    /// Latest known odometer while the charge is open, in km (R62, R65).
    #[serde(default)]
    pub last_odometer_km: Option<f64>,
    /// Whether `last_odometer_km` came from a source that rounds the odometer
    /// to whole miles (a stream frame or a sparse source), as `Some(true)`, or
    /// from a full source, as `Some(false)` (R47, R69). `None` is unknown (a
    /// blob written before this field existed, or an imported charge) and is
    /// treated as coarse.
    #[serde(default)]
    pub last_odometer_coarse: Option<bool>,
    /// In-memory child buffer (see OpenDrive::positions).
    #[serde(default)]
    pub samples: Vec<ProjectionChargeSample>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenState {
    pub id: i64,
    pub car_id: i64,
    pub state: String,
    pub start_date_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenUpdate {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
}

fn default_next_state_id() -> i64 {
    1
}

fn default_next_update_id() -> i64 {
    1
}

/// Completed entities produced since the previous open state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LifecycleDelta {
    pub drives: Vec<ProjectionDrive>,
    /// Drive IDs rejected during close. The durable path uses these to remove
    /// provisional children instead of leaving orphaned open rows.
    pub discarded_drive_ids: Vec<i64>,
    pub positions: Vec<ProjectionPosition>,
    pub charges: Vec<ProjectionCharge>,
    pub charge_samples: Vec<ProjectionChargeSample>,
    pub states: Vec<ProjectionState>,
    pub updates: Vec<ProjectionUpdate>,
    pub charge_start_coordinates: Vec<(i64, f64, f64)>,
    pub open_drive_positions: Vec<ProjectionPosition>,
    pub open_charge_samples: Vec<ProjectionChargeSample>,
}

/// One geofence as the labelling rule reads it (C1 R57, R58).
#[derive(Debug, Clone, PartialEq)]
pub struct GeofenceFence {
    /// Immutable catalogue id (`geofences.source_geofence_id`). It is the last
    /// tie-break of the overlap rule, so the chosen fence does not depend on
    /// slice order and stays stable across relabels (R57).
    pub source_geofence_id: i64,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub radius_m: f64,
    pub billing_type: Option<GeofenceBillingType>,
    pub cost_per_unit: Option<f64>,
    pub session_fee: Option<f64>,
}

/// Set the start and end geofence labels of every closed drive in `delta` from
/// its stored (already rounded) coordinates (C1 R59). A drive without
/// coordinates, or whose coordinates match no fence, gets `None`.
///
/// The label is a pure function of the stored coordinates and the fences, so
/// relabelling the same rows with the same fences gives the same labels
/// (idempotent relabel).
pub fn apply_geofence_labels(delta: &mut LifecycleDelta, fences: &[GeofenceFence]) {
    for drive in &mut delta.drives {
        drive.start_geofence = match (drive.start_latitude, drive.start_longitude) {
            (Some(latitude), Some(longitude)) => match_geofence_name(latitude, longitude, fences),
            _ => None,
        };
        drive.end_geofence = match (drive.end_latitude, drive.end_longitude) {
            (Some(latitude), Some(longitude)) => match_geofence_name(latitude, longitude, fences),
            _ => None,
        };
    }
}

/// Name of the fence [`match_geofence`] chooses for the point, if any (C1 R59).
pub fn match_geofence_name(
    latitude: f64,
    longitude: f64,
    fences: &[GeofenceFence],
) -> Option<String> {
    match_geofence(latitude, longitude, fences).map(|fence| fence.name.clone())
}

/// The one geofence rule (C1 R55 to R58), used at every label site.
///
/// - Distance: [`crate::location::great_circle_distance_m`] from the point to
///   the fence centre (R55).
/// - Boundary: a point is inside iff that distance is at most `radius_m`,
///   inclusive and with no epsilon, so a point exactly on the boundary is
///   inside and every site decides the same way (R56). "Within r metres"
///   includes r; an epsilon would make the rule differ by site.
/// - Overlap: among the fences that contain the point, the smallest radius
///   wins (a smaller fence inside a larger one is the more specific place,
///   such as a garage inside a home area), then the smallest distance to the
///   centre (the nearer centre is the more likely intended place), then the
///   smallest `source_geofence_id` (immutable, so the result does not depend on
///   slice order). Comparisons use `f64::total_cmp` (R57).
/// - Invalid input: a point that is not a valid WGS84 point matches nothing; a
///   fence whose centre is not a valid point, or whose radius is not finite or
///   is not positive, is skipped (R58). A deleted fence is absent from the
///   slice and so matches nothing.
pub fn match_geofence(
    latitude: f64,
    longitude: f64,
    fences: &[GeofenceFence],
) -> Option<&GeofenceFence> {
    let point = crate::location::Wgs84Point::new(latitude, longitude).ok()?;
    fences
        .iter()
        .filter(|fence| fence.radius_m.is_finite() && fence.radius_m > 0.0)
        .filter_map(|fence| {
            let centre = crate::location::Wgs84Point::new(fence.latitude, fence.longitude).ok()?;
            let distance = crate::location::great_circle_distance_m(point, centre);
            (distance <= fence.radius_m).then_some((fence, distance))
        })
        .min_by(|(left, left_distance), (right, right_distance)| {
            left.radius_m
                .total_cmp(&right.radius_m)
                .then_with(|| left_distance.total_cmp(right_distance))
                .then_with(|| left.source_geofence_id.cmp(&right.source_geofence_id))
        })
        .map(|(fence, _)| fence)
}

/// What a Tesla `FastCharger` type name says about the charge point (C2 R31).
///
/// `Supercharger` is Tesla's own DC network. `OtherDc` is a third-party DC
/// family (CHAdeMO, GB/T, Combo). `TeslaAc` is Tesla's own AC equipment
/// protocol (the "single wire CAN" names of wall and mobile connectors).
/// `NoInformation` is a name that says nothing (absent, empty, `<invalid>`,
/// `Unknown`, `SNA`, `Other`). `Unrecognised` is any other text, kept so a
/// name added by a later Tesla release still becomes the session's type
/// (R32) without being mistaken for evidence of AC or DC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FastChargerKind {
    Supercharger,
    OtherDc,
    TeslaAc,
    NoInformation,
    Unrecognised,
}

/// Prefix of the Fleet Telemetry enum names (`FastChargerSupercharger`); the
/// Owner API sends the plain names. Stripping at most one, case-insensitively,
/// lets both spellings classify alike, the rule C1 R40a uses for Tesla enums
/// (C2 R31).
const FAST_CHARGER_PREFIX: &str = "FastCharger";

/// Classify a charger type text (C2 R31). The text is trimmed, at most one
/// leading `FastCharger` prefix is removed (ASCII case-insensitive) and the
/// rest is compared ASCII case-insensitively with Tesla's enum names. A text
/// that is only the prefix leaves an empty rest, which is the "empty" case of
/// no information.
pub(crate) fn fast_charger_kind(fast_charger_type: Option<&str>) -> FastChargerKind {
    let Some(text) = fast_charger_type else {
        return FastChargerKind::NoInformation;
    };
    let text = text.trim();
    let rest = match text.get(..FAST_CHARGER_PREFIX.len()) {
        Some(head) if head.eq_ignore_ascii_case(FAST_CHARGER_PREFIX) => {
            &text[FAST_CHARGER_PREFIX.len()..]
        }
        _ => text,
    };
    let is = |name: &str| rest.eq_ignore_ascii_case(name);
    if is("Supercharger") {
        FastChargerKind::Supercharger
    } else if is("CHAdeMO") || is("GB") || is("Combo") {
        FastChargerKind::OtherDc
    } else if is("ACSingleWireCAN") || is("MCSingleWireCAN") {
        FastChargerKind::TeslaAc
    } else if rest.is_empty() || is("<invalid>") || is("Unknown") || is("SNA") || is("Other") {
        FastChargerKind::NoInformation
    } else {
        FastChargerKind::Unrecognised
    }
}

/// True when the type names a Tesla Supercharger (C2 R31, R41). Tesla's brand
/// on an AC wall connector or a CCS site does not make it one, so only the
/// `Supercharger` name counts.
pub(crate) fn is_supercharger(fast_charger_type: Option<&str>) -> bool {
    fast_charger_kind(fast_charger_type) == FastChargerKind::Supercharger
}

/// The tariff a charge is priced with: the billing type, the unit price and
/// an optional session fee (C2 R40). On a stored row it is the copy taken when
/// the session closed, so a later fence edit does not rewrite history (R48).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChargeTariff {
    pub billing_type: GeofenceBillingType,
    pub cost_per_unit: Option<f64>,
    pub session_fee: Option<f64>,
}

impl ChargeTariff {
    /// The tariff recorded on a charge row (C2 R40): present iff the row names
    /// a billing type. The values are checked by [`calculate_charge_cost`]
    /// (R44), so a copy with a missing or negative price reads as an unknown
    /// cost there rather than here.
    pub fn of_charge(charge: &ProjectionCharge) -> Option<ChargeTariff> {
        Some(ChargeTariff {
            billing_type: charge.billing_type?,
            cost_per_unit: charge.cost_per_unit,
            session_fee: charge.session_fee,
        })
    }

    /// The tariff of a fence, only when it is complete and valid (C2 R40): a
    /// billing type, a finite unit price of at least zero, and a session fee
    /// that is absent or finite and at least zero.
    ///
    /// A fence with a type and no price, with a fee and no type, or with a
    /// negative price or fee gives `None`, so a row never reads as if a price
    /// existed and a sign error in a fence constant never lowers a cost
    /// silently.
    pub fn of_fence(fence: &GeofenceFence) -> Option<ChargeTariff> {
        let billing_type = fence.billing_type?;
        let rate = fence.cost_per_unit?;
        if !rate.is_finite() || rate < 0.0 {
            return None;
        }
        if fence
            .session_fee
            .is_some_and(|fee| !fee.is_finite() || fee < 0.0)
        {
            return None;
        }
        Some(ChargeTariff {
            billing_type,
            cost_per_unit: Some(rate),
            session_fee: fence.session_fee,
        })
    }

    /// The same tariff with the session fee waived (C2 R44a): `Some(0.0)` when
    /// there was a fee, `None` when there was none. Used for a row that
    /// continues the previous row's plug-in, so the flat fee for one
    /// connection is charged once.
    pub fn waive_fee(self) -> ChargeTariff {
        ChargeTariff {
            session_fee: self.session_fee.map(|_| 0.0),
            ..self
        }
    }
}

/// What the cost of a charge needs besides the row and its tariff (C2 R44).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChargeCostContext {
    /// The car setting `free_supercharging` (R41).
    pub free_supercharging: bool,
    /// The row's start and end are observed, not bounds (R43a); only a
    /// per-minute cost reads it.
    pub span_observed: bool,
}

/// One minute in milliseconds, the divisor of the per-minute basis (C2 R43).
const MS_PER_MINUTE: f64 = 60_000.0;

/// Largest magnitude of a cost in hundredths, the range of the pack's scale-2
/// fixed numeric `cost_e2` (C2 R55 (c)): the pack writer is the binding
/// constraint for range, so a stored cost must never make pack writing fail.
const COST_E2_LIMIT: f64 = 99_999_999_999_999.0;

/// True when a cost, once rounded to two decimals, fits the pack's `cost_e2`
/// column (C2 R55 (c)). A non-finite value does not.
pub(crate) fn cost_fits_pack_scale(cost: f64) -> bool {
    let hundredths = (cost * 100.0).round();
    hundredths.is_finite() && hundredths.abs() <= COST_E2_LIMIT
}

/// The cost of one closed charge (C2 R44), `None` when it cannot be known.
///
/// 1. Free charging (R41): the car setting is on and the row's charger type
///    is a Supercharger: `Some(0.0)`, whatever the tariff.
/// 2. No tariff: `None` (an unknown price, not a zero price).
/// 3. A unit price that is absent, not finite or negative: `None`.
/// 4. A session fee that is not finite or negative: `None`; an absent fee is
///    no fee.
/// 5. The units: for a per-kWh tariff the energy basis of R42 (DC: the counter
///    with the integrated energy as fallback; AC or unknown: the integrated
///    inlet energy only); for a per-minute tariff the connected minutes of R43
///    when the span is observed. No units: `None`.
/// 6. `units * rate + fee`, rounded once to two decimals, or `None` when the
///    result does not fit the pack's scale (R55 (c)).
pub fn calculate_charge_cost(
    charge: &ProjectionCharge,
    tariff: Option<ChargeTariff>,
    context: ChargeCostContext,
) -> Option<f64> {
    if context.free_supercharging && is_supercharger(charge.fast_charger_type.as_deref()) {
        return Some(0.0);
    }
    let tariff = tariff?;
    let rate = tariff
        .cost_per_unit
        .filter(|rate| rate.is_finite() && *rate >= 0.0)?;
    let fee = tariff.session_fee.unwrap_or(0.0);
    if !fee.is_finite() || fee < 0.0 {
        return None;
    }
    let units = match tariff.billing_type {
        GeofenceBillingType::PerKwh => {
            let added = finite_reading(charge.charge_energy_added);
            let used = finite_reading(charge.charge_energy_used_kwh);
            match charge.is_dc {
                Some(true) => added.or(used),
                Some(false) | None => used,
            }
        }
        GeofenceBillingType::PerMinute => {
            let end = charge.end_date_ms?;
            if end < charge.start_date_ms || !context.span_observed {
                return None;
            }
            Some((end - charge.start_date_ms) as f64 / MS_PER_MINUTE)
        }
    }
    // A stored energy is never negative (R3); a negative one is a fault and
    // must not lower a cost (R55 (a)).
    .filter(|units| *units >= 0.0)?;
    let cost = round2(units * rate + fee);
    cost_fits_pack_scale(cost).then_some(cost)
}

/// The facts the cost rules read from a charge's samples (C2 R43a, R44a), kept in
/// constant space: the first sample, the last two in sample order (`timestamp_ms`,
/// then id) and the largest usable energy counter. A close or a fill folds the
/// stored samples into it one at a time, in any order, so the memory of the step
/// does not grow with the length of the charge, and it needs no sorted copy of the
/// samples. The result is the one the rules give over the sorted slice.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct ChargeSampleFacts {
    /// `(timestamp_ms, id)` of the first sample.
    first: Option<(i64, i64)>,
    /// `(timestamp_ms, id)` of the sample before the last.
    previous: Option<(i64, i64)>,
    /// `(timestamp_ms, id)` of the last sample.
    last: Option<(i64, i64)>,
    largest_counter: Option<f64>,
}

impl ChargeSampleFacts {
    /// Folds one sample in. Each sample must be offered once: the caller drops a
    /// stored sample that the delta carries again, by id.
    pub(crate) fn observe(&mut self, sample: &ProjectionChargeSample) {
        let key = (sample.timestamp_ms, sample.id);
        if self.first.is_none_or(|first| key < first) {
            self.first = Some(key);
        }
        match self.last {
            None => self.last = Some(key),
            Some(last) if key > last => {
                self.previous = Some(last);
                self.last = Some(key);
            }
            Some(_) => {
                if self.previous.is_none_or(|previous| key > previous) {
                    self.previous = Some(key);
                }
            }
        }
        if let Some(counter) = usable_energy_counter(sample) {
            self.largest_counter = Some(
                self.largest_counter
                    .map_or(counter, |best| best.max(counter)),
            );
        }
    }

    /// The facts of a slice of samples, in any order.
    #[cfg(test)]
    pub(crate) fn of(samples: &[ProjectionChargeSample]) -> Self {
        let mut facts = Self::default();
        for sample in samples {
            facts.observe(sample);
        }
        facts
    }
}

/// True when the stored times of a charge are observed, not bounds (C2 R43a).
/// `samples` are the row's samples.
///
/// With no samples the row's times are its own record (an imported row, for
/// example), so true. Otherwise false iff the first sample is more than
/// [`K_INT_MAX_MS`] after the start, or the last two samples are further apart
/// than that, or the end is more than that after the last sample. Those are
/// the unobserved stretches at either end (an after-gap start, a gap-closed
/// end); a shorter one is ordinary polling granularity, and a gap in the
/// middle that later samples close is connected time. Only stored rows and
/// samples are read, so close, fill and manual entry agree (R6). The slice form is
/// a test convenience over the production fold (`ChargeSampleFacts::of`); the folded
/// form is checked against an independently stated sorted-samples rule in
/// `r43a_r44a_the_folded_facts_equal_the_rule_over_the_sorted_samples`.
#[cfg(test)]
pub(crate) fn charge_span_observed(
    charge: &ProjectionCharge,
    samples: &[ProjectionChargeSample],
) -> bool {
    charge_span_observed_from(charge, &ChargeSampleFacts::of(samples))
}

/// [`charge_span_observed`] over folded facts.
pub(crate) fn charge_span_observed_from(
    charge: &ProjectionCharge,
    facts: &ChargeSampleFacts,
) -> bool {
    let (Some((first, _)), Some((last, _))) = (facts.first, facts.last) else {
        return true;
    };
    if first.saturating_sub(charge.start_date_ms) > K_INT_MAX_MS {
        return false;
    }
    if let Some((previous, _)) = facts.previous
        && last.saturating_sub(previous) > K_INT_MAX_MS
    {
        return false;
    }
    if let Some(end) = charge.end_date_ms
        && end.saturating_sub(last) > K_INT_MAX_MS
    {
        return false;
    }
    true
}

/// True when a Hub row continues the previous row's plug-in (C2 R44a): its
/// energy added is known and the largest usable counter among its samples
/// exceeds it by at least [`K_ENERGY_EPS_KWH`].
///
/// In a live session `E = m - b`, so `m - E` is the baseline, and the test
/// holds exactly when the baseline was non-zero and kept (a top-up after
/// `Complete` with no unplug, drive or movement; the second row of a
/// continuity split). A baseline lowered to zero, an unknown counter or a
/// restart carry makes it false, and the session fee is then charged as for
/// any session. The slice form is a test convenience over the production fold
/// (`ChargeSampleFacts::of`); the folded form is checked against an independently
/// stated sorted-samples rule in
/// `r43a_r44a_the_folded_facts_equal_the_rule_over_the_sorted_samples`.
#[cfg(test)]
pub(crate) fn charge_continues_plug_in(
    charge: &ProjectionCharge,
    samples: &[ProjectionChargeSample],
) -> bool {
    charge_continues_plug_in_from(charge, &ChargeSampleFacts::of(samples))
}

/// [`charge_continues_plug_in`] over folded facts.
pub(crate) fn charge_continues_plug_in_from(
    charge: &ProjectionCharge,
    facts: &ChargeSampleFacts,
) -> bool {
    let Some(added) = finite_reading(charge.charge_energy_added) else {
        return false;
    };
    facts
        .largest_counter
        .is_some_and(|largest| largest - added >= K_ENERGY_EPS_KWH)
}

#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleStep {
    pub state: OpenSessionState,
    pub delta: LifecycleDelta,
    pub quarantined: bool,
}
