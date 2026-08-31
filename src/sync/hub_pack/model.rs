// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeofenceBillingType {
    PerKwh,
    PerMinute,
}

impl GeofenceBillingType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PerKwh => "per_kwh",
            Self::PerMinute => "per_minute",
        }
    }
}

/// Exact physical `unit_of_length` labels from TeslaMate global settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionUnitOfLengthV2_2 {
    #[serde(rename = "km")]
    Kilometers,
    #[serde(rename = "mi")]
    Miles,
}

impl ProjectionUnitOfLengthV2_2 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kilometers => "km",
            Self::Miles => "mi",
        }
    }
}

/// Exact physical `unit_of_temperature` labels from TeslaMate global settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionUnitOfTemperatureV2_2 {
    #[serde(rename = "C")]
    Celsius,
    #[serde(rename = "F")]
    Fahrenheit,
}

impl ProjectionUnitOfTemperatureV2_2 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Celsius => "C",
            Self::Fahrenheit => "F",
        }
    }
}

/// Exact physical `unit_of_pressure` labels from TeslaMate global settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionUnitOfPressureV2_2 {
    #[serde(rename = "bar")]
    Bar,
    #[serde(rename = "psi")]
    Psi,
}

impl ProjectionUnitOfPressureV2_2 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Psi => "psi",
        }
    }
}

/// Exact physical TeslaMate `range` enum labels from global settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionPreferredRangeV2_2 {
    #[serde(rename = "ideal")]
    Ideal,
    #[serde(rename = "rated")]
    Rated,
}

impl ProjectionPreferredRangeV2_2 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ideal => "ideal",
            Self::Rated => "rated",
        }
    }
}

/// Exact local representation of a constrained PostgreSQL `numeric(p,s)`.
///
/// PostgreSQL accepts `NaN` for constrained numeric columns. A finite source
/// value is scaled to its contract-specific integer exponent; `NaN` remains a
/// distinct tagged state, and nullable source fields use `Option` around this
/// enum so SQL `NULL` is never conflated with `NaN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionFixedNumericV2_2 {
    Finite(i64),
    NaN,
}

impl ProjectionFixedNumericV2_2 {
    const fn sqlite_parts(self) -> (Option<i64>, i64) {
        match self {
            Self::Finite(value) => (Some(value), 0),
            Self::NaN => (None, 1),
        }
    }
}

const fn optional_fixed_numeric_sqlite_parts(
    value: Option<ProjectionFixedNumericV2_2>,
) -> (Option<i64>, i64) {
    match value {
        Some(value) => value.sqlite_parts(),
        None => (None, 0),
    }
}

/// Exact bits of a PostgreSQL `double precision` value. SQLite REAL would
/// canonicalize values such as `-0.0` and NaN payloads, so schema 2.2 stores
/// the big-endian IEEE-754 bits as an eight-byte BLOB instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionFloat64BitsV2_2(pub u64);

impl ProjectionFloat64BitsV2_2 {
    pub const fn from_f64(value: f64) -> Self {
        Self(value.to_bits())
    }

    pub const fn to_be_bytes(self) -> [u8; 8] {
        self.0.to_be_bytes()
    }
}

impl FromStr for GeofenceBillingType {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "per_kwh" => Ok(Self::PerKwh),
            "per_minute" => Ok(Self::PerMinute),
            _ => Err(()),
        }
    }
}

impl FromStr for ProjectionUnitOfLengthV2_2 {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "km" => Ok(Self::Kilometers),
            "mi" => Ok(Self::Miles),
            _ => Err(()),
        }
    }
}

impl FromStr for ProjectionUnitOfTemperatureV2_2 {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "C" => Ok(Self::Celsius),
            "F" => Ok(Self::Fahrenheit),
            _ => Err(()),
        }
    }
}

impl FromStr for ProjectionUnitOfPressureV2_2 {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "bar" => Ok(Self::Bar),
            "psi" => Ok(Self::Psi),
            _ => Err(()),
        }
    }
}

impl FromStr for ProjectionPreferredRangeV2_2 {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ideal" => Ok(Self::Ideal),
            "rated" => Ok(Self::Rated),
            _ => Err(()),
        }
    }
}

/// The stable Hub identities a pack is bound to. One pack is for one vehicle
/// and one local mirror car ID, not an account-wide database copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionBinding {
    pub installation_id: Uuid,
    pub account_id: Uuid,
    pub vehicle_id: Uuid,
    pub generation: u64,
    pub selected_car_id: i64,
}

/// One complete, projected mirror image. Incremental change packs will be a
/// separate format: this first writer refuses to invent tombstone semantics.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectionSnapshot {
    pub cars: Vec<ProjectionCar>,
    pub drives: Vec<ProjectionDrive>,
    pub positions: Vec<ProjectionPosition>,
    pub charges: Vec<ProjectionCharge>,
    pub charge_samples: Vec<ProjectionChargeSample>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionDeltaEntity {
    Car,
    CarSetting,
    Geofence,
    Address,
    Drive,
    Position,
    Charge,
    ChargeSample,
    State,
    Update,
}

impl ProjectionDeltaEntity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Car => "car",
            Self::CarSetting => "car_setting",
            Self::Geofence => "geofence",
            Self::Address => "address",
            Self::Drive => "drive",
            Self::Position => "position",
            Self::Charge => "charge",
            Self::ChargeSample => "charge_sample",
            Self::State => "state",
            Self::Update => "update",
        }
    }

    /// Canonical insertion order for source-owned tombstones. It writes
    /// dependent rows before their parents without changing the pinned SQLite
    /// table layout; consumers need an explicit query order for application
    /// sequencing after loading a pack.
    const fn source_owned_tombstone_order(self) -> Option<u8> {
        match self {
            Self::ChargeSample => Some(0),
            Self::Position => Some(1),
            Self::Charge => Some(2),
            Self::Drive => Some(3),
            Self::State => Some(4),
            Self::Update => Some(5),
            Self::Car | Self::CarSetting | Self::Geofence | Self::Address => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionTombstone {
    pub entity: ProjectionDeltaEntity,
    pub id: i64,
    pub car_id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionCarSettingsPatch {
    pub car_id: i64,
    pub settings: ProjectionCarSettings,
}

/// Sparse typed changes. Missing rows mean unchanged rows in the external
/// base lineage; they are never interpreted as deletes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionDelta {
    pub binding: ProjectionBinding,
    pub sequence: SequenceRange,
    pub parent_digest: Sha256Digest,
    pub cars: Vec<ProjectionCar>,
    pub car_settings: Vec<ProjectionCarSettingsPatch>,
    pub drives: Vec<ProjectionDrive>,
    pub positions: Vec<ProjectionPosition>,
    pub charges: Vec<ProjectionCharge>,
    pub charge_samples: Vec<ProjectionChargeSample>,
    pub states: Vec<ProjectionState>,
    pub updates: Vec<ProjectionUpdate>,
    pub tombstones: Vec<ProjectionTombstone>,
}

impl ProjectionDelta {
    /// True when this delta contains no rows in any logical stream.
    pub(crate) fn is_empty(&self) -> bool {
        self.row_count().is_ok_and(|row_count| row_count == 0)
    }

    fn row_count(&self) -> Result<u64, ProjectionPackError> {
        [
            self.cars.len(),
            self.car_settings.len(),
            self.drives.len(),
            self.positions.len(),
            self.charges.len(),
            self.charge_samples.len(),
            self.states.len(),
            self.updates.len(),
            self.tombstones.len(),
        ]
        .into_iter()
        .try_fold(0_u64, |total, count| {
            total
                .checked_add(u64::try_from(count).map_err(|_| ProjectionPackError::TooManyRows)?)
                .ok_or(ProjectionPackError::TooManyRows)
        })
    }
}

#[derive(Debug, Clone)]
pub struct ProjectionDeltaPackRequest<'a> {
    pub pack_id: Uuid,
    pub snapshot_id: Uuid,
    pub ordinal: u32,
    pub delta: &'a ProjectionDelta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectionCarSettings {
    pub enabled: bool,
    pub use_streaming_api: bool,
    pub suspend_after_idle_min: i64,
    pub suspend_min: i64,
    #[serde(default = "default_suspend_min_resolved")]
    pub suspend_min_resolved: bool,
    pub req_not_unlocked: bool,
    pub free_supercharging: bool,
    pub lfp_battery: bool,
}

impl Default for ProjectionCarSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            use_streaming_api: true,
            suspend_after_idle_min: 15,
            suspend_min: 21,
            suspend_min_resolved: true,
            req_not_unlocked: false,
            free_supercharging: false,
            lfp_battery: false,
        }
    }
}

fn default_suspend_min_resolved() -> bool {
    true
}

impl ProjectionCarSettings {
    pub fn new_live() -> Self {
        Self {
            suspend_min_resolved: false,
            ..Self::default()
        }
    }
}

/// Validate the source model before schema-specific projection. Schema 2.0
/// does not materialise companion settings, but a full snapshot must still
/// reject impossible embedded settings before it creates output.
fn validate_car_settings(settings: &ProjectionCarSettings) -> Result<(), ProjectionPackError> {
    if settings.suspend_after_idle_min <= 0 || settings.suspend_min <= 0 {
        return Err(invalid("car settings durations must be positive"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionState {
    pub id: i64,
    pub car_id: i64,
    pub state: String,
    pub start_date_ms: i64,
    pub end_date_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionUpdate {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
    pub end_date_ms: i64,
    pub version: String,
}

impl ProjectionSnapshot {
    fn row_count(&self) -> Result<u64, ProjectionPackError> {
        [
            self.cars.len(),
            self.drives.len(),
            self.positions.len(),
            self.charges.len(),
            self.charge_samples.len(),
        ]
        .into_iter()
        .try_fold(0_u64, |total, count| {
            total
                .checked_add(u64::try_from(count).map_err(|_| ProjectionPackError::TooManyRows)?)
                .ok_or(ProjectionPackError::TooManyRows)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionCar {
    pub id: i64,
    pub name: String,
    pub model: String,
    pub vin: Option<String>,
    #[serde(default)]
    pub source_eid: Option<i64>,
    #[serde(default)]
    pub source_vid: Option<i64>,
    #[serde(default)]
    pub trim_badging: Option<String>,
    #[serde(default)]
    pub marketing_name: Option<String>,
    #[serde(default)]
    pub exterior_color: Option<String>,
    #[serde(default)]
    pub wheel_type: Option<String>,
    #[serde(default)]
    pub spoiler_type: Option<String>,
    pub firmware_version: Option<String>,
    pub efficiency_wh_per_km: Option<f64>,
    #[serde(default)]
    pub settings: ProjectionCarSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectionCarPatch {
    pub name: Option<String>,
    pub model: Option<String>,
    pub vin: Option<String>,
    pub trim_badging: Option<String>,
    pub marketing_name: Option<String>,
    pub exterior_color: Option<String>,
    pub wheel_type: Option<String>,
    pub spoiler_type: Option<String>,
    pub firmware_version: Option<String>,
}

impl ProjectionCarPatch {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.model.is_none()
            && self.vin.is_none()
            && self.trim_badging.is_none()
            && self.marketing_name.is_none()
            && self.exterior_color.is_none()
            && self.wheel_type.is_none()
            && self.spoiler_type.is_none()
            && self.firmware_version.is_none()
    }

    pub fn merge_newer(&mut self, newer: &Self) {
        if newer.name.is_some() {
            self.name = newer.name.clone();
        }
        if newer.model.is_some() {
            self.model = newer.model.clone();
        }
        if newer.vin.is_some() {
            self.vin = newer.vin.clone();
        }
        if newer.trim_badging.is_some() {
            self.trim_badging = newer.trim_badging.clone();
        }
        if newer.marketing_name.is_some() {
            self.marketing_name = newer.marketing_name.clone();
        }
        if newer.exterior_color.is_some() {
            self.exterior_color = newer.exterior_color.clone();
        }
        if newer.wheel_type.is_some() {
            self.wheel_type = newer.wheel_type.clone();
        }
        if newer.spoiler_type.is_some() {
            self.spoiler_type = newer.spoiler_type.clone();
        }
        if newer.firmware_version.is_some() {
            self.firmware_version = newer.firmware_version.clone();
        }
    }

    pub fn into_car(
        &self,
        car_id: i64,
        existing: Option<&ProjectionCar>,
        fallback_name: Option<String>,
        fallback_vin: Option<String>,
    ) -> ProjectionCar {
        ProjectionCar {
            id: car_id,
            name: self
                .name
                .clone()
                .or_else(|| existing.map(|car| car.name.clone()))
                .or(fallback_name)
                .unwrap_or_else(|| "Tesla".to_owned()),
            model: self
                .model
                .clone()
                .or_else(|| existing.map(|car| car.model.clone()))
                .unwrap_or_else(|| "Unknown Tesla".to_owned()),
            vin: self
                .vin
                .clone()
                .or_else(|| existing.and_then(|car| car.vin.clone()))
                .or(fallback_vin),
            source_eid: existing.and_then(|car| car.source_eid),
            source_vid: existing.and_then(|car| car.source_vid),
            trim_badging: self
                .trim_badging
                .clone()
                .or_else(|| existing.and_then(|car| car.trim_badging.clone())),
            marketing_name: self
                .marketing_name
                .clone()
                .or_else(|| existing.and_then(|car| car.marketing_name.clone())),
            exterior_color: self
                .exterior_color
                .clone()
                .or_else(|| existing.and_then(|car| car.exterior_color.clone())),
            wheel_type: self
                .wheel_type
                .clone()
                .or_else(|| existing.and_then(|car| car.wheel_type.clone())),
            spoiler_type: self
                .spoiler_type
                .clone()
                .or_else(|| existing.and_then(|car| car.spoiler_type.clone())),
            firmware_version: self
                .firmware_version
                .clone()
                .or_else(|| existing.and_then(|car| car.firmware_version.clone())),
            efficiency_wh_per_km: existing.and_then(|car| car.efficiency_wh_per_km),
            settings: existing.map(|car| car.settings.clone()).unwrap_or_default(),
        }
    }
}

pub fn normalize_tesla_model_code(value: &str) -> String {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    let compact = lower.replace(' ', "");
    if compact.starts_with("models") || compact == "lychee" {
        "S".to_owned()
    } else if compact.starts_with("model3") {
        "3".to_owned()
    } else if compact.starts_with("modelx") || compact == "tamarind" {
        "X".to_owned()
    } else if compact.starts_with("modely") {
        "Y".to_owned()
    } else if compact.starts_with("cybertruck") {
        "Cybertruck".to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub fn teslamate_suspend_min_default(
    model: Option<&str>,
    trim_badging: Option<&str>,
    marketing_name: Option<&str>,
) -> Option<i64> {
    match normalize_tesla_model_code(model?).as_str() {
        "3" | "Y" | "Cybertruck" => Some(12),
        "S" | "X" if trim_badging.is_none() || marketing_name.is_some() => Some(12),
        "S" | "X" => Some(21),
        _ => None,
    }
}

pub fn normalize_tesla_trim(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

pub fn derive_tesla_marketing_name(
    model: &str,
    trim_badging: Option<&str>,
    raw_car_type: Option<&str>,
    vin: Option<&str>,
) -> Option<String> {
    let model = normalize_tesla_model_code(model);
    let trim = trim_badging.map(normalize_tesla_trim);
    let raw = raw_car_type.unwrap_or_default().to_ascii_lowercase();
    match (model.as_str(), trim.as_deref(), raw.as_str()) {
        ("S", Some("100D"), "lychee") => Some("LR".to_owned()),
        ("S", Some("P100D"), "lychee") => Some("Plaid".to_owned()),
        ("S", Some("100D"), "models2") => Some("LR+".to_owned()),
        ("3", Some("P74D"), _) => Some("LR AWD Performance".to_owned()),
        ("3", Some("74D"), _) => Some("LR AWD".to_owned()),
        ("3", Some("74"), _) => Some("LR".to_owned()),
        ("3", Some("62"), _) => Some("MR".to_owned()),
        ("3", Some("50"), _) => Some(model_3_base_trim(vin).to_owned()),
        ("X", Some("100D"), "tamarind") => Some("LR".to_owned()),
        ("X", Some("P100D"), "tamarind") => Some("Plaid".to_owned()),
        ("Y", Some("P74D"), _) => Some("LR AWD Performance".to_owned()),
        ("Y", Some("74D"), _) => Some("LR AWD".to_owned()),
        ("Y", Some("74"), _) => Some("LR".to_owned()),
        ("Y", Some("50"), _) => Some("SR".to_owned()),
        _ => None,
    }
}

fn model_3_base_trim(vin: Option<&str>) -> &'static str {
    let Some(vin) = vin.filter(|vin| vin.len() == 17 && vin.is_ascii()) else {
        return "SR+";
    };
    let model_year = match vin.as_bytes()[9] {
        b'A' => 2010,
        b'B' => 2011,
        b'C' => 2012,
        b'D' => 2013,
        b'E' => 2014,
        b'F' => 2015,
        b'G' => 2016,
        b'H' => 2017,
        b'J' => 2018,
        b'K' => 2019,
        b'L' => 2020,
        b'M' => 2021,
        b'N' => 2022,
        b'P' => 2023,
        b'R' => 2024,
        b'S' => 2025,
        b'T' => 2026,
        b'V' => 2027,
        b'W' => 2028,
        b'X' => 2029,
        b'Y' => 2030,
        b'1'..=b'9' => 2030 + i32::from(vin.as_bytes()[9] - b'0'),
        _ => return "SR+",
    };
    if model_year >= 2022 { "RWD" } else { "SR+" }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionDrive {
    pub id: i64,
    pub car_id: i64,
    pub optimized_at_ms: Option<i64>,
    pub start_date_ms: i64,
    pub end_date_ms: i64,
    pub distance_km: Option<f64>,
    pub duration_min: Option<i64>,
    pub efficiency: Option<f64>,
    pub outside_temp_avg: Option<f64>,
    #[serde(default)]
    pub inside_temp_avg: Option<f64>,
    pub speed_max: Option<i64>,
    #[serde(default)]
    pub power_max: Option<f64>,
    #[serde(default)]
    pub power_min: Option<f64>,
    #[serde(default)]
    pub start_ideal_range_km: Option<f64>,
    #[serde(default)]
    pub end_ideal_range_km: Option<f64>,
    pub start_address: Option<String>,
    pub end_address: Option<String>,
    pub start_geofence: Option<String>,
    pub end_geofence: Option<String>,
    pub start_latitude: Option<f64>,
    pub start_longitude: Option<f64>,
    pub end_latitude: Option<f64>,
    pub end_longitude: Option<f64>,
    pub start_soc: Option<i64>,
    pub end_soc: Option<i64>,
    pub start_rated_range_km: Option<f64>,
    pub end_rated_range_km: Option<f64>,
    #[serde(default)]
    pub ascent: Option<i64>,
    #[serde(default)]
    pub descent: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionPosition {
    pub id: i64,
    pub drive_id: Option<i64>,
    pub car_id: i64,
    pub date_ms: i64,
    pub latitude: f64,
    pub longitude: f64,
    pub speed: Option<i64>,
    pub power: Option<f64>,
    pub battery_level: Option<i64>,
    pub usable_battery_level: Option<i64>,
    pub elevation: Option<i64>,
    pub odometer: Option<f64>,
    pub ideal_battery_range_km: Option<f64>,
    #[serde(default)]
    pub est_battery_range_km: Option<f64>,
    #[serde(default)]
    pub rated_battery_range_km: Option<f64>,
    #[serde(default)]
    pub fan_status: Option<i64>,
    #[serde(default)]
    pub driver_temp_setting: Option<f64>,
    #[serde(default)]
    pub passenger_temp_setting: Option<f64>,
    #[serde(default)]
    pub is_climate_on: Option<bool>,
    #[serde(default)]
    pub is_rear_defroster_on: Option<bool>,
    #[serde(default)]
    pub is_front_defroster_on: Option<bool>,
    #[serde(default)]
    pub inside_temp: Option<f64>,
    #[serde(default)]
    pub outside_temp: Option<f64>,
    #[serde(default)]
    pub battery_heater: Option<bool>,
    #[serde(default)]
    pub battery_heater_on: Option<bool>,
    #[serde(default)]
    pub battery_heater_no_power: Option<bool>,
    #[serde(default)]
    pub tpms_pressure_fl: Option<f64>,
    #[serde(default)]
    pub tpms_pressure_fr: Option<f64>,
    #[serde(default)]
    pub tpms_pressure_rl: Option<f64>,
    #[serde(default)]
    pub tpms_pressure_rr: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionCharge {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
    pub end_date_ms: Option<i64>,
    pub charge_energy_added: Option<f64>,
    #[serde(default)]
    pub charge_energy_used_kwh: Option<f64>,
    #[serde(default)]
    pub start_ideal_range_km: Option<f64>,
    #[serde(default)]
    pub end_ideal_range_km: Option<f64>,
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub fast_charger_type: Option<String>,
    #[serde(default)]
    pub billing_type: Option<GeofenceBillingType>,
    #[serde(default)]
    pub cost_per_unit: Option<f64>,
    #[serde(default)]
    pub session_fee: Option<f64>,
    #[serde(default)]
    pub start_latitude: Option<f64>,
    #[serde(default)]
    pub start_longitude: Option<f64>,
    pub start_battery_level: Option<i64>,
    pub end_battery_level: Option<i64>,
    pub duration_min: Option<i64>,
    pub address: Option<String>,
    pub location_name: Option<String>,
    pub geofence: Option<String>,
    pub is_dc: Option<bool>,
    pub charge_rate_km_per_hour: Option<f64>,
    pub max_charger_power_kw: Option<f64>,
    pub outside_temp_avg: Option<f64>,
    pub start_rated_range_km: Option<f64>,
    pub end_rated_range_km: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionChargeSample {
    pub id: i64,
    pub charge_process_id: i64,
    pub timestamp_ms: i64,
    pub battery_level: Option<i64>,
    pub usable_battery_level: Option<i64>,
    pub charge_energy_added_kwh: Option<f64>,
    pub charger_power_kw: Option<f64>,
    pub charger_voltage: Option<f64>,
    pub charger_actual_current: Option<f64>,
    pub charger_pilot_current: Option<f64>,
    pub charger_phases: Option<i64>,
    pub ideal_range_km: Option<f64>,
    pub rated_range_km: Option<f64>,
    pub outside_temp_c: Option<f64>,
    pub battery_heater_on: Option<bool>,
    pub battery_heater: Option<bool>,
    pub battery_heater_no_power: Option<bool>,
    pub not_enough_power_to_heat: Option<bool>,
    pub fast_charger_present: Option<bool>,
    pub fast_charger_brand: Option<String>,
    pub fast_charger_type: Option<String>,
    pub charge_cable: Option<String>,
}

/// Exact selected-car `car_settings` physical values for the schema-2.2
/// local candidate. This is deliberately distinct from the compatibility
/// `ProjectionCarSettings`, which resolves source defaults and has no source
/// `id` identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionCarSettingsV2_2 {
    pub id: i64,
    pub suspend_min: i32,
    pub suspend_after_idle_min: i32,
    pub req_not_unlocked: bool,
    pub free_supercharging: bool,
    pub use_streaming_api: bool,
    pub enabled: bool,
    pub lfp_battery: bool,
}

/// Exact selected-car physical values for the schema-2.2 local candidate.
/// In particular, this retains source integer widths, optional source text,
/// timestamp(0) PostgreSQL binary microseconds, and source `efficiency`
/// unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionCarV2_2 {
    pub id: i16,
    pub eid: i64,
    pub vid: i64,
    pub vin: Option<String>,
    pub name: Option<String>,
    pub model: Option<String>,
    pub efficiency: Option<f64>,
    pub trim_badging: Option<String>,
    pub marketing_name: Option<String>,
    pub exterior_color: Option<String>,
    pub wheel_type: Option<String>,
    pub spoiler_type: Option<String>,
    pub display_priority: i16,
    pub inserted_at_pg_us: i64,
    pub updated_at_pg_us: i64,
    pub settings_id: i64,
}

/// Exact physical `states_status` labels in the schema-2.2 local candidate.
/// This is separate from the legacy string projection so the local writer
/// cannot silently broaden the reviewed source enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectionStateStatusV2_2 {
    Online,
    Offline,
    Asleep,
}

impl ProjectionStateStatusV2_2 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Asleep => "asleep",
        }
    }
}

/// Exact physical `states` source row for the schema-2.2 local candidate.
/// Timestamp fields are PostgreSQL binary timestamp microseconds relative to
/// 2000-01-01, retained as raw signed i64 values including infinity sentinels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionStateV2_2 {
    pub id: i32,
    pub car_id: i16,
    pub state: ProjectionStateStatusV2_2,
    pub start_date_pg_us: i64,
    pub end_date_pg_us: Option<i64>,
}

/// Exact physical `updates` source row for the schema-2.2 local candidate.
/// Timestamp fields retain PostgreSQL binary timestamp microseconds verbatim;
/// nullable end/version values deliberately receive no interval, trim, or
/// default policy at this boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionUpdateV2_2 {
    pub id: i32,
    pub car_id: i16,
    pub start_date_pg_us: i64,
    pub end_date_pg_us: Option<i64>,
    pub version: Option<String>,
}

/// A normalized, selected-car-referenced TeslaMate address for the schema-2.2
/// full snapshot.  The source `addresses.raw` payload has no representation at
/// this boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionAddressV2_2 {
    pub id: i32,
    pub display_name: Option<String>,
    pub latitude_e6: Option<ProjectionFixedNumericV2_2>,
    pub longitude_e6: Option<ProjectionFixedNumericV2_2>,
    pub name: Option<String>,
    pub house_number: Option<String>,
    pub road: Option<String>,
    pub neighbourhood: Option<String>,
    pub city: Option<String>,
    pub county: Option<String>,
    pub postcode: Option<String>,
    pub state: Option<String>,
    pub state_district: Option<String>,
    pub country: Option<String>,
    pub inserted_at_pg_us: i64,
    pub updated_at_pg_us: i64,
    pub osm_id: Option<i64>,
    pub osm_type: Option<String>,
}

/// A normalized, selected-car-referenced TeslaMate geofence for the
/// schema-2.2 local candidate.  Its source numerics use exact fixed scales:
/// latitude/longitude e6, cost-per-unit e4, and session-fee e2.  `radius`
/// preserves the source `smallint` verbatim, including zero and signed bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionGeofenceV2_2 {
    pub id: i32,
    pub name: String,
    pub latitude_e6: ProjectionFixedNumericV2_2,
    pub longitude_e6: ProjectionFixedNumericV2_2,
    pub radius: i16,
    pub billing_type: GeofenceBillingType,
    pub cost_per_unit_e4: Option<ProjectionFixedNumericV2_2>,
    pub session_fee_e2: Option<ProjectionFixedNumericV2_2>,
    pub inserted_at_pg_us: i64,
    pub updated_at_pg_us: i64,
}

/// Exact physical TeslaMate `drives` values for the schema-2.2 local
/// candidate. The compatibility `ProjectionDrive` is intentionally separate:
/// this type retains signed source identities, open/end-before-start rows,
/// raw PostgreSQL timestamps, tagged source NUMERIC values, and FLOAT8 bits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionDriveV2_2 {
    pub id: i32,
    pub car_id: i16,
    pub start_date_pg_us: i64,
    pub end_date_pg_us: Option<i64>,
    pub start_position_id: Option<i32>,
    pub end_position_id: Option<i32>,
    pub start_address_id: Option<i32>,
    pub end_address_id: Option<i32>,
    pub start_geofence_id: Option<i32>,
    pub end_geofence_id: Option<i32>,
    pub outside_temp_avg_e1: Option<ProjectionFixedNumericV2_2>,
    pub inside_temp_avg_e1: Option<ProjectionFixedNumericV2_2>,
    pub speed_max: Option<i16>,
    pub power_max: Option<i16>,
    pub power_min: Option<i16>,
    pub start_ideal_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub end_ideal_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub start_rated_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub end_rated_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub start_km: Option<ProjectionFloat64BitsV2_2>,
    pub end_km: Option<ProjectionFloat64BitsV2_2>,
    pub distance: Option<ProjectionFloat64BitsV2_2>,
    pub duration_min: Option<i16>,
    pub ascent: Option<i16>,
    pub descent: Option<i16>,
}

/// Exact physical TeslaMate `positions` values for the schema-2.2 local
/// candidate. The compatibility `ProjectionPosition` remains separate: this
/// type retains signed source identities, raw PostgreSQL timestamps, tagged
/// NUMERIC values, and exact FLOAT8 odometer bits without semantic policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionPositionV2_2 {
    pub id: i32,
    pub car_id: i16,
    pub drive_id: Option<i32>,
    pub date_pg_us: i64,
    pub latitude_e6: ProjectionFixedNumericV2_2,
    pub longitude_e6: ProjectionFixedNumericV2_2,
    pub elevation: Option<i16>,
    pub speed: Option<i16>,
    pub power: Option<i16>,
    pub odometer: Option<ProjectionFloat64BitsV2_2>,
    pub ideal_battery_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub est_battery_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub rated_battery_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub battery_level: Option<i16>,
    pub usable_battery_level: Option<i16>,
    pub battery_heater: Option<bool>,
    pub battery_heater_on: Option<bool>,
    pub battery_heater_no_power: Option<bool>,
    pub outside_temp_e1: Option<ProjectionFixedNumericV2_2>,
    pub inside_temp_e1: Option<ProjectionFixedNumericV2_2>,
    pub fan_status: Option<i32>,
    pub driver_temp_setting_e1: Option<ProjectionFixedNumericV2_2>,
    pub passenger_temp_setting_e1: Option<ProjectionFixedNumericV2_2>,
    pub is_climate_on: Option<bool>,
    pub is_rear_defroster_on: Option<bool>,
    pub is_front_defroster_on: Option<bool>,
    pub tpms_pressure_fl_e1: Option<ProjectionFixedNumericV2_2>,
    pub tpms_pressure_fr_e1: Option<ProjectionFixedNumericV2_2>,
    pub tpms_pressure_rl_e1: Option<ProjectionFixedNumericV2_2>,
    pub tpms_pressure_rr_e1: Option<ProjectionFixedNumericV2_2>,
}

/// Exact physical TeslaMate `charging_processes` values for the schema-2.2
/// local candidate. Compatibility charge summaries are deliberately absent:
/// this type keeps raw source IDs, timestamps, tagged NUMERIC values, and
/// nullable source fields without interval, SOC, or relation-closure policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionChargingProcessV2_2 {
    pub id: i32,
    pub car_id: i16,
    pub position_id: i32,
    pub address_id: Option<i32>,
    pub geofence_id: Option<i32>,
    pub start_date_pg_us: i64,
    pub end_date_pg_us: Option<i64>,
    pub charge_energy_added_e2: Option<ProjectionFixedNumericV2_2>,
    pub charge_energy_used_e2: Option<ProjectionFixedNumericV2_2>,
    pub start_ideal_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub end_ideal_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub start_rated_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub end_rated_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub start_battery_level: Option<i16>,
    pub end_battery_level: Option<i16>,
    pub duration_min: Option<i16>,
    pub outside_temp_avg_e1: Option<ProjectionFixedNumericV2_2>,
    pub cost_e2: Option<ProjectionFixedNumericV2_2>,
}

/// Exact physical TeslaMate `charges` values for the schema-2.2 local
/// candidate. These are individual source samples, not normalized charge
/// sessions; tri-state booleans, source widths, and tagged NUMERIC values are
/// retained verbatim. The selected-car reader scopes rows through an extant
/// charging process; source constraint state is not re-attested by V3 SQLite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionChargeV2_2 {
    pub id: i32,
    pub charging_process_id: i32,
    pub date_pg_us: i64,
    pub battery_heater: Option<bool>,
    pub battery_heater_on: Option<bool>,
    pub battery_heater_no_power: Option<bool>,
    pub battery_level: Option<i16>,
    pub usable_battery_level: Option<i16>,
    pub charge_energy_added_e2: ProjectionFixedNumericV2_2,
    pub charger_actual_current: Option<i16>,
    pub charger_phases: Option<i16>,
    pub charger_pilot_current: Option<i16>,
    pub charger_power: i16,
    pub charger_voltage: Option<i16>,
    pub conn_charge_cable: Option<String>,
    pub fast_charger_present: Option<bool>,
    pub fast_charger_brand: Option<String>,
    pub fast_charger_type: Option<String>,
    pub ideal_battery_range_km_e2: ProjectionFixedNumericV2_2,
    pub rated_battery_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub not_enough_power_to_heat: Option<bool>,
    pub outside_temp_e1: Option<ProjectionFixedNumericV2_2>,
}

/// Exact physical source-wide TeslaMate `settings` singleton for the schema-2.2
/// local candidate. It deliberately remains independent of a selected car:
/// URLs stay opaque nullable source text, language/theme keep their physical
/// text domain, and timestamp(0) values retain raw PostgreSQL microseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionGlobalSettingsV2_2 {
    pub id: i64,
    pub unit_of_length: ProjectionUnitOfLengthV2_2,
    pub unit_of_temperature: ProjectionUnitOfTemperatureV2_2,
    pub unit_of_pressure: ProjectionUnitOfPressureV2_2,
    pub preferred_range: ProjectionPreferredRangeV2_2,
    pub base_url: Option<String>,
    pub grafana_url: Option<String>,
    pub language: String,
    pub theme_mode: String,
    pub inserted_at_pg_us: i64,
    pub updated_at_pg_us: i64,
}

/// A separate full-only schema-2.2 local candidate snapshot. It intentionally
/// does not reuse `ProjectionSnapshot`: 2.0/2.1 carry flattened labels, while
/// 2.2 carries exact local physical rows. Selected-car source scope is checked
/// in Rust; this V3 local schema deliberately emits no SQLite FKs. Some source
/// targets can be omitted from a selected-car subset, so physical references
/// are not invented into local graph closure.
/// This does not claim a complete field-contract mapping or publication eligibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionSnapshotV2_2 {
    pub global_settings: Vec<ProjectionGlobalSettingsV2_2>,
    pub cars: Vec<ProjectionCarV2_2>,
    pub car_settings: Vec<ProjectionCarSettingsV2_2>,
    pub addresses: Vec<ProjectionAddressV2_2>,
    pub geofences: Vec<ProjectionGeofenceV2_2>,
    pub drives: Vec<ProjectionDriveV2_2>,
    pub positions: Vec<ProjectionPositionV2_2>,
    pub charging_processes: Vec<ProjectionChargingProcessV2_2>,
    pub charges: Vec<ProjectionChargeV2_2>,
    pub states: Vec<ProjectionStateV2_2>,
    pub updates: Vec<ProjectionUpdateV2_2>,
}

impl ProjectionSnapshotV2_2 {
    fn row_count(&self) -> Result<u64, ProjectionPackError> {
        [
            self.global_settings.len(),
            self.cars.len(),
            self.car_settings.len(),
            self.addresses.len(),
            self.geofences.len(),
            self.drives.len(),
            self.positions.len(),
            self.charging_processes.len(),
            self.charges.len(),
            self.states.len(),
            self.updates.len(),
        ]
        .into_iter()
        .try_fold(0_u64, |total, count| {
            total
                .checked_add(u64::try_from(count).map_err(|_| ProjectionPackError::TooManyRows)?)
                .ok_or(ProjectionPackError::TooManyRows)
        })
    }
}

/// Input for one immutable, full Hub projection pack.
#[derive(Debug, Clone)]
pub struct ProjectionPackRequest<'a> {
    pub pack_id: Uuid,
    pub snapshot_id: Uuid,
    pub ordinal: u32,
    pub binding: ProjectionBinding,
    pub sequence: SequenceRange,
    pub snapshot: &'a ProjectionSnapshot,
}

/// Input for one locally validated schema-2.2 full snapshot. Signing is
/// available; catalogue publication is a separate HubStore call.
#[derive(Debug, Clone)]
pub struct ProjectionPackRequestV2_2<'a> {
    pub pack_id: Uuid,
    pub snapshot_id: Uuid,
    pub ordinal: u32,
    pub binding: ProjectionBinding,
    pub sequence: SequenceRange,
    pub snapshot: &'a ProjectionSnapshotV2_2,
}

impl ProjectionPackRequestV2_2<'_> {
    /// Bind an already verified schema-2.2 object to a signed full-snapshot
    /// manifest. Catalogue publication remains a separate store call.
    pub fn signed_manifest(
        &self,
        built: &BuiltProjectionPack,
        cursor_key: &CursorKey,
    ) -> Result<SyncManifest, ProjectionPackError> {
        if built.metadata.pack_id != self.pack_id
            || built.metadata.snapshot_id != self.snapshot_id
            || built.metadata.ordinal != self.ordinal
            || built.metadata.schema != HUB_PROJECTION_SCHEMA_V3
            || built.metadata.format != PackFormat::HubProjectionSqlite
            || built.metadata.sequence != self.sequence
            || built.metadata.row_count != self.snapshot.row_count()?
        {
            return Err(invalid(
                "built schema 2.2 pack does not match its signed request",
            ));
        }
        signed_full_snapshot_manifest(
            &self.binding,
            self.snapshot_id,
            self.sequence,
            std::slice::from_ref(built),
            self.snapshot.row_count()?,
            cursor_key,
        )
    }
}

impl ProjectionPackRequest<'_> {
    /// Bind an already verified typed object to a manifest and a cursor signed
    /// by this installation. Publication is deliberately separate so the
    /// caller can put the object in the local catalog atomically afterwards.
    pub fn signed_manifest(
        &self,
        built: &BuiltProjectionPack,
        cursor_key: &CursorKey,
    ) -> Result<SyncManifest, ProjectionPackError> {
        if built.metadata.pack_id != self.pack_id
            || built.metadata.snapshot_id != self.snapshot_id
            || built.metadata.ordinal != self.ordinal
            || built.metadata.schema != HUB_PROJECTION_SCHEMA_V1
            || built.metadata.format != PackFormat::HubProjectionSqlite
            || built.metadata.sequence != self.sequence
            || built.metadata.row_count != self.snapshot.row_count()?
        {
            return Err(invalid("built pack does not match its signed request"));
        }
        signed_full_snapshot_manifest(
            &self.binding,
            self.snapshot_id,
            self.sequence,
            std::slice::from_ref(built),
            self.snapshot.row_count()?,
            cursor_key,
        )
    }

    pub fn signed_manifest_with_states_and_updates(
        &self,
        built: &BuiltProjectionPack,
        states: &[ProjectionState],
        updates: &[ProjectionUpdate],
        cursor_key: &CursorKey,
    ) -> Result<SyncManifest, ProjectionPackError> {
        let row_count = row_count_with_states_and_updates(self.snapshot, states, updates)?;
        if built.metadata.pack_id != self.pack_id
            || built.metadata.snapshot_id != self.snapshot_id
            || built.metadata.ordinal != self.ordinal
            || built.metadata.schema != HUB_PROJECTION_SCHEMA_V2
            || built.metadata.format != PackFormat::HubProjectionSqlite
            || built.metadata.sequence != self.sequence
            || built.metadata.row_count != row_count
        {
            return Err(invalid("built V2 pack does not match its signed request"));
        }
        signed_full_snapshot_manifest(
            &self.binding,
            self.snapshot_id,
            self.sequence,
            std::slice::from_ref(built),
            row_count,
            cursor_key,
        )
    }
}
