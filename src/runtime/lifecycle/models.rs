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
    /// Durable bookend retained when an offline drive times out before the
    /// first authoritative online sample can prove a TeslaMate-style
    /// gained-range charge.
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
        if let Some(temp) = position.outside_temp {
            open.outside_temp_sum += temp;
            open.outside_temp_count = open.outside_temp_count.saturating_add(1);
        }
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
        if let Some(energy) = sample.charge_energy_added_kwh {
            open.first_energy_added.get_or_insert(energy);
            open.max_energy_added =
                Some(open.max_energy_added.map_or(energy, |max| max.max(energy)));
            open.last_energy_added = Some(energy);
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
        if let Some(is_dc) = sample.fast_charger_present {
            open.is_dc = Some(open.is_dc.unwrap_or(false) || is_dc);
        }
        if sample.fast_charger_type.is_some() {
            open.fast_charger_type = sample.fast_charger_type.clone();
        }
        if let Some(temp) = sample.outside_temp_c {
            open.outside_temp_sum += temp;
            open.outside_temp_count = open.outside_temp_count.saturating_add(1);
        }
        observe_charge_sample(&mut open, &sample);
    }
    open.energy_used_kwh = row.charge_energy_used_kwh.or(open.energy_used_kwh);
    if open.first_energy_added.is_none() {
        open.first_energy_added = row.charge_energy_added;
        open.max_energy_added = row.charge_energy_added;
        open.last_energy_added = row.charge_energy_added;
    }
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
    /// TeslaMate only synthesizes an offline charge after a drive has gone
    /// through `{:driving, {:offline, last}, _}`. Persist that fact so an
    /// online GPS gap cannot invent the same charge.
    #[serde(default)]
    pub saw_offline: bool,
    /// Last Owner/Fleet `charge_energy_added` observed while this drive was
    /// open. Stream frames omit the field. Used as TeslaMate's "last" bookend
    /// for a gained-range synthetic charge.
    #[serde(default)]
    pub last_charge_energy_added: Option<f64>,
    /// Last ideal range from any sample, including those without odometer.
    /// TeslaMate compares `last_response.charge_state.ideal_battery_range`.
    #[serde(default)]
    pub last_ideal_range_km: Option<f64>,
    /// In-memory child buffer for pure unit tests and single-batch close. The
    /// durable db path clears this before encoding and never rehydrates the full
    /// history into active state on every observation.
    #[serde(default)]
    pub positions: Vec<ProjectionPosition>,
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

#[derive(Debug, Clone, PartialEq)]
pub struct GeofenceFence {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub radius_m: f64,
    pub billing_type: Option<GeofenceBillingType>,
    pub cost_per_unit: Option<f64>,
    pub session_fee: Option<f64>,
}

pub fn apply_geofence_labels(delta: &mut LifecycleDelta, fences: &[GeofenceFence]) {
    for drive in &mut delta.drives {
        if drive.start_geofence.is_none() {
            drive.start_geofence = match (drive.start_latitude, drive.start_longitude) {
                (Some(latitude), Some(longitude)) => {
                    match_geofence_name(latitude, longitude, fences)
                }
                _ => None,
            };
        }
        if drive.end_geofence.is_none() {
            drive.end_geofence = match (drive.end_latitude, drive.end_longitude) {
                (Some(latitude), Some(longitude)) => {
                    match_geofence_name(latitude, longitude, fences)
                }
                _ => None,
            };
        }
    }
}

pub fn match_geofence_name(
    latitude: f64,
    longitude: f64,
    fences: &[GeofenceFence],
) -> Option<String> {
    match_geofence(latitude, longitude, fences).map(|fence| fence.name.clone())
}

pub fn match_geofence(
    latitude: f64,
    longitude: f64,
    fences: &[GeofenceFence],
) -> Option<&GeofenceFence> {
    fences
        .iter()
        .filter_map(|fence| {
            let distance = haversine_m(latitude, longitude, fence.latitude, fence.longitude);
            (distance <= fence.radius_m).then_some((distance, fence))
        })
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, fence)| fence)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChargeTariff {
    pub billing_type: GeofenceBillingType,
    pub cost_per_unit: Option<f64>,
    pub session_fee: Option<f64>,
}

pub fn calculate_charge_cost(
    fast_charger_type: Option<&str>,
    free_supercharging: bool,
    charge_energy_added: Option<f64>,
    charge_energy_used_kwh: Option<f64>,
    duration_min: Option<i64>,
    tariff: Option<ChargeTariff>,
) -> Option<f64> {
    if free_supercharging && fast_charger_type.is_some_and(|value| value.starts_with("Tesla")) {
        return Some(0.0);
    }
    let tariff = tariff?;
    let fee = tariff
        .session_fee
        .filter(|value| value.is_finite())
        .unwrap_or(0.0);
    match tariff.billing_type {
        GeofenceBillingType::PerKwh => {
            let energy = [charge_energy_added, charge_energy_used_kwh]
                .into_iter()
                .flatten()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .reduce(f64::max)?;
            let variable = tariff
                .cost_per_unit
                .filter(|value| value.is_finite())
                .map_or(0.0, |rate| energy * rate);
            (variable.is_finite()
                && (variable != 0.0
                    || tariff.cost_per_unit.is_some()
                    || tariff.session_fee.is_some()))
            .then_some(variable + fee)
        }
        GeofenceBillingType::PerMinute => {
            let minutes = duration_min.filter(|value| *value >= 0)? as f64;
            let rate = tariff.cost_per_unit.filter(|value| value.is_finite())?;
            let cost = minutes * rate + fee;
            cost.is_finite().then_some(cost)
        }
    }
}

#[cfg(test)]
mod tariff_tests {
    use super::*;

    fn per_kwh(rate: Option<f64>, fee: Option<f64>) -> ChargeTariff {
        ChargeTariff {
            billing_type: GeofenceBillingType::PerKwh,
            cost_per_unit: rate,
            session_fee: fee,
        }
    }

    #[test]
    fn cost_precedence_and_nulls_match_teslamate() {
        assert_eq!(
            calculate_charge_cost(
                Some("Tesla Supercharger"),
                true,
                Some(10.0),
                Some(12.0),
                Some(30),
                Some(per_kwh(Some(0.30), Some(2.0))),
            ),
            Some(0.0)
        );
        assert_eq!(
            calculate_charge_cost(
                None,
                false,
                Some(10.0),
                Some(12.0),
                Some(30),
                Some(per_kwh(Some(0.30), Some(2.0))),
            ),
            Some(5.6)
        );
        assert_eq!(
            calculate_charge_cost(
                None,
                false,
                Some(-1.0),
                Some(10.0),
                Some(30),
                Some(per_kwh(Some(0.30), None)),
            ),
            Some(3.0)
        );
        assert_eq!(
            calculate_charge_cost(
                None,
                false,
                None,
                None,
                Some(30),
                Some(per_kwh(Some(0.30), Some(2.0))),
            ),
            None
        );
        assert_eq!(
            calculate_charge_cost(
                None,
                false,
                None,
                None,
                Some(30),
                Some(ChargeTariff {
                    billing_type: GeofenceBillingType::PerMinute,
                    cost_per_unit: Some(0.10),
                    session_fee: Some(2.0),
                }),
            ),
            Some(5.0)
        );
        assert_eq!(
            calculate_charge_cost(None, false, Some(10.0), None, Some(30), None),
            None
        );
    }
}

fn haversine_m(latitude_a: f64, longitude_a: f64, latitude_b: f64, longitude_b: f64) -> f64 {
    let radius = 6_371_000.0;
    let lat = (latitude_b - latitude_a).to_radians();
    let lon = (longitude_b - longitude_a).to_radians();
    let a = (lat / 2.0).sin().powi(2)
        + latitude_a.to_radians().cos() * latitude_b.to_radians().cos() * (lon / 2.0).sin().powi(2);
    radius * 2.0 * a.sqrt().asin()
}

#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleStep {
    pub state: OpenSessionState,
    pub delta: LifecycleDelta,
    pub quarantined: bool,
}
