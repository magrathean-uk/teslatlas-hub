//! Typed, bounded SQLite packs for the Teslatlas Hub source.
//!
//! This deliberately does not turn arbitrary Hub observations into rows for a
//! phone. A producer must first create this checked projection. The resulting
//! SQLite file has the five source-owned tables that the Teslatlas core mirror
//! understands, plus a small binding record that prevents cross-account or
//! cross-vehicle activation.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    str::FromStr,
};

use rusqlite::{Connection, OpenFlags, params};
use rustix::fs::statvfs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::protocol::{
    CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V1, MirrorTable, OpaqueCursor, PROTOCOL_V1,
    PackCompression, PackFormat, ProtocolError, ProtocolLimits,
    SQLITE_HUB_PROJECTION_APPLICATION_ID, SchemaVersion, SequenceRange, Sha256Digest, SyncManifest,
    TransportPack, VerifiedTransportPack,
};

const COMPRESSION_LEVEL: i32 = 3;
const MAX_TEXT_BYTES: usize = 16 * 1024;

/// The first additive projection schema. Schema 2.0 remains the default and
/// is never widened in place.
pub const HUB_PROJECTION_SCHEMA_V2: SchemaVersion = SchemaVersion { major: 2, minor: 1 };

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
                .checked_add(u64::try_from(count).map_err(|_| ProjectionPackError::TooManyRows)? )
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
        "3" | "Y" => Some(12),
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
        ("3", Some("50"), _) => Some("SR+".to_owned()),
        ("X", Some("100D"), "tamarind") => Some("LR".to_owned()),
        ("X", Some("P100D"), "tamarind") => Some("Plaid".to_owned()),
        ("Y", Some("P74D"), _) => Some("LR AWD Performance".to_owned()),
        ("Y", Some("74D"), _) => Some("LR AWD".to_owned()),
        ("Y", Some("74"), _) => Some("LR".to_owned()),
        ("Y", Some("50"), _) => Some("SR".to_owned()),
        _ => None,
    }
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

/// Sign a full-snapshot manifest from several already-verified typed chunks.
///
/// Large history is intentionally represented by several independently
/// resumable SQLite objects, not one host-memory-sized database. Every chunk
/// repeats its required parent rows (the selected car and any parents of its
/// children), so it remains a valid foreign-key-checked SQLite database by
/// itself. The iOS importer stages all chunks before it atomically activates
/// the complete mirror.
pub fn signed_full_snapshot_manifest(
    binding: &ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    chunks: &[BuiltProjectionPack],
    total_rows: u64,
    cursor_key: &CursorKey,
) -> Result<SyncManifest, ProjectionPackError> {
    validate_binding(binding)?;
    if snapshot_id.is_nil() {
        return Err(invalid("snapshot ID must not be nil"));
    }
    if !sequence.is_ordered() {
        return Err(invalid("full snapshot sequence is unordered"));
    }
    if chunks.is_empty() {
        return Err(invalid("full snapshot needs at least one chunk"));
    }

    let schema = chunks[0].metadata.schema;
    if schema != HUB_PROJECTION_SCHEMA_V1 && schema != HUB_PROJECTION_SCHEMA_V2 {
        return Err(invalid("unsupported projection schema"));
    }
    let mut total_compressed_bytes = 0_u64;
    let mut total_uncompressed_bytes = 0_u64;
    let mut transport_rows = 0_u64;
    let mut metadata = Vec::with_capacity(chunks.len());
    for (expected_ordinal, built) in chunks.iter().enumerate() {
        let pack = &built.metadata;
        if pack.snapshot_id != snapshot_id
            || pack.schema != schema
            || pack.format != PackFormat::HubProjectionSqlite
            || pack.sequence != sequence
            || pack.ordinal
                != u32::try_from(expected_ordinal)
                    .map_err(|_| ProjectionPackError::TooManyChunks)?
        {
            return Err(invalid("built chunk does not match its snapshot manifest"));
        }
        total_compressed_bytes = total_compressed_bytes
            .checked_add(pack.compressed_bytes)
            .ok_or(ProjectionPackError::ManifestTotalsOverflow)?;
        total_uncompressed_bytes = total_uncompressed_bytes
            .checked_add(pack.uncompressed_bytes)
            .ok_or(ProjectionPackError::ManifestTotalsOverflow)?;
        transport_rows = transport_rows
            .checked_add(pack.row_count)
            .ok_or(ProjectionPackError::ManifestTotalsOverflow)?;
        metadata.push(pack.clone());
    }

    if total_rows == 0 || total_rows > transport_rows {
        return Err(invalid("logical row total exceeds transport rows"));
    }
    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: sequence.to_inclusive,
        },
    )?;
    let manifest = SyncManifest {
        protocol: PROTOCOL_V1,
        schema,
        installation_id: binding.installation_id,
        account_id: binding.account_id,
        vehicle_id: binding.vehicle_id,
        generation: binding.generation,
        snapshot_id,
        mode: crate::protocol::TransferMode::FullSnapshot,
        base_sequence: sequence.from_exclusive,
        head_sequence: sequence.to_inclusive,
        chunk_count: u32::try_from(metadata.len())
            .map_err(|_| ProjectionPackError::TooManyChunks)?,
        total_compressed_bytes,
        total_uncompressed_bytes,
        total_rows,
        chunks: metadata,
        terminal_cursor,
    };
    manifest.validate()?;
    manifest.validate_terminal_cursor(cursor_key)?;
    Ok(manifest)
}

/// A complete, verified immutable object ready for the existing pack catalog.
#[derive(Debug, Clone)]
pub struct BuiltProjectionPack {
    pub metadata: TransportPack,
    pub path: PathBuf,
    pub verified: VerifiedTransportPack,
}

#[derive(Debug, Clone)]
pub struct ProjectionPackWriter {
    packs_dir: PathBuf,
    limits: ProtocolLimits,
    minimum_free_bytes: u64,
}

impl ProjectionPackWriter {
    pub fn new(packs_dir: impl Into<PathBuf>) -> Self {
        Self {
            packs_dir: packs_dir.into(),
            limits: ProtocolLimits::default(),
            minimum_free_bytes: 0,
        }
    }

    pub fn with_minimum_free_bytes(mut self, minimum_free_bytes: u64) -> Self {
        self.minimum_free_bytes = minimum_free_bytes;
        self
    }

    pub fn with_limits(packs_dir: impl Into<PathBuf>, limits: ProtocolLimits) -> Self {
        Self {
            packs_dir: packs_dir.into(),
            limits,
            minimum_free_bytes: 0,
        }
    }

    pub fn content_path(&self, digest: Sha256Digest) -> PathBuf {
        self.packs_dir
            .join("sha256")
            .join(format!("{digest}.sqlite.zst"))
    }

    /// Refuse a source capture unless there is room for every permitted final
    /// pack, the active SQLite/compression pair, and the caller's free-space
    /// reserve. The limit is intentionally worst-case: a later full snapshot
    /// must never consume the reserve while replacing an earlier one.
    pub fn ensure_full_snapshot_capacity(
        &self,
        minimum_free_bytes: u64,
    ) -> Result<(), ProjectionPackError> {
        let final_bytes = u64::try_from(self.limits.max_chunks)
            .map_err(|_| ProjectionPackError::CapacityOverflow)?
            .checked_mul(self.limits.max_compressed_pack_bytes)
            .ok_or(ProjectionPackError::CapacityOverflow)?;
        let required = final_bytes
            .checked_add(self.transient_write_bytes()?)
            .and_then(|value| value.checked_add(minimum_free_bytes))
            .ok_or(ProjectionPackError::CapacityOverflow)?;
        self.ensure_free_bytes(required)
    }

    /// Write and verify an immutable, complete mirror snapshot. The caller
    /// supplies a bounded projection; the writer never inspects raw telemetry.
    pub fn write_full_snapshot(
        &self,
        request: &ProjectionPackRequest<'_>,
    ) -> Result<BuiltProjectionPack, ProjectionPackError> {
        validate_request(request, self.limits)?;
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        let staging_dir = self.packs_dir.join(".staging");
        let content_dir = self.packs_dir.join("sha256");
        fs::create_dir_all(&staging_dir).map_err(|source| {
            ProjectionPackError::CreateDirectory {
                path: staging_dir.clone(),
                source,
            }
        })?;
        fs::create_dir_all(&content_dir).map_err(|source| {
            ProjectionPackError::CreateDirectory {
                path: content_dir.clone(),
                source,
            }
        })?;

        let sqlite_temp = StagedFile::create(&staging_dir, "projection.sqlite")?;
        write_projection_sqlite(
            sqlite_temp.path(),
            request,
            self.limits,
            HUB_PROJECTION_SCHEMA_V1,
            &[],
            &[],
            request.snapshot.row_count()?,
        )?;
        let uncompressed_bytes = fs::metadata(sqlite_temp.path())
            .map_err(|source| ProjectionPackError::Metadata {
                path: sqlite_temp.path().to_path_buf(),
                source,
            })?
            .len();

        let compressed_temp = StagedFile::create(&staging_dir, "projection.zst")?;
        let (sha256, compressed_bytes) = compress_file(sqlite_temp.path(), compressed_temp.path())?;
        let metadata = TransportPack {
            pack_id: request.pack_id,
            snapshot_id: request.snapshot_id,
            ordinal: request.ordinal,
            schema: HUB_PROJECTION_SCHEMA_V1,
            format: PackFormat::HubProjectionSqlite,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(sha256),
            sha256,
            compressed_bytes,
            uncompressed_bytes,
            row_count: request.snapshot.row_count()?,
            sequence: request.sequence,
            tables: tables_for_snapshot(request.snapshot, false),
        };
        metadata.validate(self.limits)?;
        let verified = verify_file(&metadata, compressed_temp.path(), self.limits)?;
        let final_path = self.content_path(sha256);
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        publish_immutable(compressed_temp.path(), &final_path, &metadata, self.limits)?;

        Ok(BuiltProjectionPack {
            metadata,
            path: final_path,
            verified,
        })
    }

    /// Write the additive state-history projection. The original writer and
    /// its schema remain unchanged; callers must opt into this entry point.
    pub fn write_full_snapshot_with_states(
        &self,
        request: &ProjectionPackRequest<'_>,
        states: &[ProjectionState],
    ) -> Result<BuiltProjectionPack, ProjectionPackError> {
        self.write_full_snapshot_with_states_and_updates(request, states, &[])
    }

    pub fn write_full_snapshot_with_states_and_updates(
        &self,
        request: &ProjectionPackRequest<'_>,
        states: &[ProjectionState],
        updates: &[ProjectionUpdate],
    ) -> Result<BuiltProjectionPack, ProjectionPackError> {
        validate_request(request, self.limits)?;
        validate_states(states, request.binding.selected_car_id)?;
        validate_updates(updates, request.binding.selected_car_id)?;
        let row_count = row_count_with_states_and_updates(request.snapshot, states, updates)?;
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        let staging_dir = self.packs_dir.join(".staging");
        let content_dir = self.packs_dir.join("sha256");
        fs::create_dir_all(&staging_dir).map_err(|source| {
            ProjectionPackError::CreateDirectory {
                path: staging_dir.clone(),
                source,
            }
        })?;
        fs::create_dir_all(&content_dir).map_err(|source| {
            ProjectionPackError::CreateDirectory {
                path: content_dir.clone(),
                source,
            }
        })?;

        let sqlite_temp = StagedFile::create(&staging_dir, "projection.sqlite")?;
        write_projection_sqlite(
            sqlite_temp.path(),
            request,
            self.limits,
            HUB_PROJECTION_SCHEMA_V2,
            states,
            updates,
            row_count,
        )?;
        let uncompressed_bytes = fs::metadata(sqlite_temp.path())
            .map_err(|source| ProjectionPackError::Metadata {
                path: sqlite_temp.path().to_path_buf(),
                source,
            })?
            .len();
        let compressed_temp = StagedFile::create(&staging_dir, "projection.zst")?;
        let (sha256, compressed_bytes) = compress_file(sqlite_temp.path(), compressed_temp.path())?;
        let metadata = TransportPack {
            pack_id: request.pack_id,
            snapshot_id: request.snapshot_id,
            ordinal: request.ordinal,
            schema: HUB_PROJECTION_SCHEMA_V2,
            format: PackFormat::HubProjectionSqlite,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(sha256),
            sha256,
            compressed_bytes,
            uncompressed_bytes,
            row_count,
            sequence: request.sequence,
            tables: tables_for_snapshot(request.snapshot, true),
        };
        metadata.validate(self.limits)?;
        let verified = verify_file(&metadata, compressed_temp.path(), self.limits)?;
        let final_path = self.content_path(sha256);
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        publish_immutable(compressed_temp.path(), &final_path, &metadata, self.limits)?;
        Ok(BuiltProjectionPack {
            metadata,
            path: final_path,
            verified,
        })
    }

    /// Write one sparse schema-2.1 delta. This path creates only the schema;
    /// it never reads or copies the external base lineage.
    pub fn write_delta(
        &self,
        request: &ProjectionDeltaPackRequest<'_>,
    ) -> Result<BuiltProjectionPack, ProjectionPackError> {
        let row_count = validate_delta(request, self.limits)?;
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        let staging_dir = self.packs_dir.join(".staging");
        let content_dir = self.packs_dir.join("sha256");
        fs::create_dir_all(&staging_dir).map_err(|source| ProjectionPackError::CreateDirectory {
            path: staging_dir.clone(),
            source,
        })?;
        fs::create_dir_all(&content_dir).map_err(|source| ProjectionPackError::CreateDirectory {
            path: content_dir.clone(),
            source,
        })?;

        let sqlite_temp = StagedFile::create(&staging_dir, "projection-delta.sqlite")?;
        let empty = ProjectionSnapshot {
            cars: Vec::new(),
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
        };
        let schema_request = ProjectionPackRequest {
            pack_id: request.pack_id,
            snapshot_id: request.snapshot_id,
            ordinal: request.ordinal,
            binding: request.delta.binding.clone(),
            sequence: request.delta.sequence,
            snapshot: &empty,
        };
        write_projection_sqlite(
            sqlite_temp.path(),
            &schema_request,
            self.limits,
            HUB_PROJECTION_SCHEMA_V2,
            &[],
            &[],
            0,
        )?;
        write_delta_rows(sqlite_temp.path(), request, self.limits, row_count)?;

        let uncompressed_bytes = fs::metadata(sqlite_temp.path())
            .map_err(|source| ProjectionPackError::Metadata {
                path: sqlite_temp.path().to_path_buf(),
                source,
            })?
            .len();
        let compressed_temp = StagedFile::create(&staging_dir, "projection-delta.zst")?;
        let (sha256, compressed_bytes) = compress_file(sqlite_temp.path(), compressed_temp.path())?;
        let metadata = TransportPack {
            pack_id: request.pack_id,
            snapshot_id: request.snapshot_id,
            ordinal: request.ordinal,
            schema: HUB_PROJECTION_SCHEMA_V2,
            format: PackFormat::HubProjectionSqlite,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(sha256),
            sha256,
            compressed_bytes,
            uncompressed_bytes,
            row_count,
            sequence: request.delta.sequence,
            tables: tables_for_delta(request.delta),
        };
        metadata.validate(self.limits)?;
        let verified = verify_file(&metadata, compressed_temp.path(), self.limits)?;
        let final_path = self.content_path(sha256);
        publish_immutable(compressed_temp.path(), &final_path, &metadata, self.limits)?;
        Ok(BuiltProjectionPack {
            metadata,
            path: final_path,
            verified,
        })
    }

    fn transient_write_bytes(&self) -> Result<u64, ProjectionPackError> {
        self.limits
            .max_uncompressed_pack_bytes
            .checked_mul(2)
            .ok_or(ProjectionPackError::CapacityOverflow)
    }

    fn ensure_free_bytes(&self, required: u64) -> Result<(), ProjectionPackError> {
        let staging_dir = self.packs_dir.join(".staging");
        fs::create_dir_all(&staging_dir).map_err(|source| {
            ProjectionPackError::CreateDirectory {
                path: staging_dir.clone(),
                source,
            }
        })?;
        let available = available_bytes(&staging_dir)?;
        if available < required {
            return Err(ProjectionPackError::InsufficientFreeSpace {
                required,
                available,
            });
        }
        Ok(())
    }
}

fn validate_request(
    request: &ProjectionPackRequest<'_>,
    limits: ProtocolLimits,
) -> Result<(), ProjectionPackError> {
    if request.pack_id.is_nil() || request.snapshot_id.is_nil() {
        return Err(invalid("pack and snapshot IDs must not be nil"));
    }
    validate_binding(&request.binding)?;
    if !request.sequence.is_ordered() {
        return Err(invalid("full snapshot sequence is unordered"));
    }
    if request.snapshot.row_count()? > limits.max_rows_per_pack {
        return Err(ProjectionPackError::TooManyRows);
    }
    if request.snapshot.cars.len() != 1 {
        return Err(invalid(
            "one vehicle projection must contain exactly one car",
        ));
    }

    let car = &request.snapshot.cars[0];
    require_positive(car.id, "car.id")?;
    if car.id != request.binding.selected_car_id {
        return Err(invalid("selected_car_id does not match car.id"));
    }
    validate_required_text(&car.name, "car.name")?;
    validate_required_text(&car.model, "car.model")?;
    validate_optional_text(car.vin.as_deref(), "car.vin")?;
    validate_optional_text(car.firmware_version.as_deref(), "car.firmware_version")?;
    validate_optional_nonnegative(car.efficiency_wh_per_km, "car.efficiency_wh_per_km")?;

    let mut drive_ids = HashSet::with_capacity(request.snapshot.drives.len());
    for drive in &request.snapshot.drives {
        require_unique_positive(&mut drive_ids, drive.id, "drive.id")?;
        require_same_car(
            drive.car_id,
            request.binding.selected_car_id,
            "drive.car_id",
        )?;
        validate_interval(drive.start_date_ms, drive.end_date_ms, "drive")?;
        validate_optional_positive(drive.optimized_at_ms, "drive.optimized_at_ms")?;
        validate_optional_nonnegative(drive.distance_km, "drive.distance_km")?;
        validate_optional_nonnegative(drive.efficiency, "drive.efficiency")?;
        validate_optional_nonnegative(drive.start_rated_range_km, "drive.start_rated_range_km")?;
        validate_optional_nonnegative(drive.end_rated_range_km, "drive.end_rated_range_km")?;
        validate_optional_finite(drive.outside_temp_avg, "drive.outside_temp_avg")?;
        validate_coordinate_pair(drive.start_latitude, drive.start_longitude, "drive.start")?;
        validate_coordinate_pair(drive.end_latitude, drive.end_longitude, "drive.end")?;
        validate_optional_soc(drive.start_soc, "drive.start_soc")?;
        validate_optional_soc(drive.end_soc, "drive.end_soc")?;
        for (value, name) in [
            (drive.start_address.as_deref(), "drive.start_address"),
            (drive.end_address.as_deref(), "drive.end_address"),
            (drive.start_geofence.as_deref(), "drive.start_geofence"),
            (drive.end_geofence.as_deref(), "drive.end_geofence"),
        ] {
            validate_optional_text(value, name)?;
        }
    }

    let mut charge_ids = HashSet::with_capacity(request.snapshot.charges.len());
    for charge in &request.snapshot.charges {
        require_unique_positive(&mut charge_ids, charge.id, "charge.id")?;
        require_same_car(
            charge.car_id,
            request.binding.selected_car_id,
            "charge.car_id",
        )?;
        require_positive(charge.start_date_ms, "charge.start_date_ms")?;
        if charge
            .end_date_ms
            .is_some_and(|end| end < charge.start_date_ms)
        {
            return Err(invalid("charge.end_date_ms precedes charge.start_date_ms"));
        }
        validate_optional_nonnegative(charge.charge_energy_added, "charge.charge_energy_added")?;
        validate_optional_finite(charge.cost, "charge.cost")?;
        validate_optional_finite(charge.cost_per_unit, "charge.cost_per_unit")?;
        validate_optional_finite(charge.session_fee, "charge.session_fee")?;
        validate_optional_nonnegative(
            charge.charge_energy_used_kwh,
            "charge.charge_energy_used_kwh",
        )?;
        validate_optional_nonnegative(
            charge.charge_rate_km_per_hour,
            "charge.charge_rate_km_per_hour",
        )?;
        validate_optional_nonnegative(charge.max_charger_power_kw, "charge.max_charger_power_kw")?;
        validate_optional_nonnegative(charge.start_rated_range_km, "charge.start_rated_range_km")?;
        validate_optional_nonnegative(charge.end_rated_range_km, "charge.end_rated_range_km")?;
        validate_optional_finite(charge.outside_temp_avg, "charge.outside_temp_avg")?;
        validate_optional_soc(charge.start_battery_level, "charge.start_battery_level")?;
        validate_optional_soc(charge.end_battery_level, "charge.end_battery_level")?;
        for (value, name) in [
            (charge.address.as_deref(), "charge.address"),
            (charge.location_name.as_deref(), "charge.location_name"),
            (charge.geofence.as_deref(), "charge.geofence"),
            (
                charge.fast_charger_type.as_deref(),
                "charge.fast_charger_type",
            ),
        ] {
            validate_optional_text(value, name)?;
        }
    }

    let mut position_ids = HashSet::with_capacity(request.snapshot.positions.len());
    for position in &request.snapshot.positions {
        require_unique_positive(&mut position_ids, position.id, "position.id")?;
        require_same_car(
            position.car_id,
            request.binding.selected_car_id,
            "position.car_id",
        )?;
        require_positive(position.date_ms, "position.date_ms")?;
        if let Some(drive_id) = position.drive_id
            && !drive_ids.contains(&drive_id)
        {
            return Err(invalid("position.drive_id is not present in this pack"));
        }
        validate_coordinate(position.latitude, position.longitude, "position")?;
        validate_optional_soc(position.battery_level, "position.battery_level")?;
        validate_optional_soc(
            position.usable_battery_level,
            "position.usable_battery_level",
        )?;
        validate_optional_nonnegative(position.odometer, "position.odometer")?;
        validate_optional_nonnegative(
            position.ideal_battery_range_km,
            "position.ideal_battery_range_km",
        )?;
        validate_optional_nonnegative(
            position.rated_battery_range_km,
            "position.rated_battery_range_km",
        )?;
        validate_optional_finite(position.inside_temp, "position.inside_temp")?;
        validate_optional_finite(position.outside_temp, "position.outside_temp")?;
    }

    let mut sample_ids = HashSet::with_capacity(request.snapshot.charge_samples.len());
    for sample in &request.snapshot.charge_samples {
        require_unique_positive(&mut sample_ids, sample.id, "charge_sample.id")?;
        require_positive(sample.timestamp_ms, "charge_sample.timestamp_ms")?;
        if !charge_ids.contains(&sample.charge_process_id) {
            return Err(invalid(
                "charge_sample.charge_process_id is not present in this pack",
            ));
        }
        validate_optional_soc(sample.battery_level, "charge_sample.battery_level")?;
        validate_optional_soc(
            sample.usable_battery_level,
            "charge_sample.usable_battery_level",
        )?;
        for (value, name) in [
            (
                sample.charge_energy_added_kwh,
                "charge_sample.charge_energy_added_kwh",
            ),
            (sample.charger_power_kw, "charge_sample.charger_power_kw"),
            (sample.charger_voltage, "charge_sample.charger_voltage"),
            (
                sample.charger_actual_current,
                "charge_sample.charger_actual_current",
            ),
            (
                sample.charger_pilot_current,
                "charge_sample.charger_pilot_current",
            ),
            (sample.ideal_range_km, "charge_sample.ideal_range_km"),
            (sample.rated_range_km, "charge_sample.rated_range_km"),
        ] {
            validate_optional_nonnegative(value, name)?;
        }
        validate_optional_finite(sample.outside_temp_c, "charge_sample.outside_temp_c")?;
        for (value, name) in [
            (
                sample.fast_charger_brand.as_deref(),
                "charge_sample.fast_charger_brand",
            ),
            (
                sample.fast_charger_type.as_deref(),
                "charge_sample.fast_charger_type",
            ),
            (sample.charge_cable.as_deref(), "charge_sample.charge_cable"),
        ] {
            validate_optional_text(value, name)?;
        }
    }
    Ok(())
}

fn validate_delta(
    request: &ProjectionDeltaPackRequest<'_>,
    limits: ProtocolLimits,
) -> Result<u64, ProjectionPackError> {
    if request.pack_id.is_nil() || request.snapshot_id.is_nil() {
        return Err(invalid("delta pack and snapshot IDs must not be nil"));
    }
    let delta = request.delta;
    validate_binding(&delta.binding)?;
    if delta.sequence.to_inclusive <= delta.sequence.from_exclusive {
        return Err(invalid("delta sequence must make forward progress"));
    }
    if delta.parent_digest.is_zero() {
        return Err(invalid("delta parent digest must not be zero"));
    }
    let selected_car_id = delta.binding.selected_car_id;
    let mut car_ids = HashSet::with_capacity(delta.cars.len());
    for car in &delta.cars {
        require_unique_positive(&mut car_ids, car.id, "car.id")?;
        require_same_car(car.id, selected_car_id, "car.id")?;
        validate_required_text(&car.name, "car.name")?;
        validate_required_text(&car.model, "car.model")?;
        validate_optional_text(car.vin.as_deref(), "car.vin")?;
        validate_optional_text(car.firmware_version.as_deref(), "car.firmware_version")?;
        validate_optional_nonnegative(car.efficiency_wh_per_km, "car.efficiency_wh_per_km")?;
    }
    let mut setting_ids = HashSet::with_capacity(delta.car_settings.len());
    for patch in &delta.car_settings {
        require_unique_positive(&mut setting_ids, patch.car_id, "car_settings.car_id")?;
        require_same_car(patch.car_id, selected_car_id, "car_settings.car_id")?;
        if car_ids.contains(&patch.car_id) {
            return Err(invalid("car upsert and car settings patch overlap"));
        }
        if patch.settings.suspend_after_idle_min <= 0 || patch.settings.suspend_min <= 0 {
            return Err(invalid("car settings durations must be positive"));
        }
    }

    let mut drive_ids = HashSet::with_capacity(delta.drives.len());
    for drive in &delta.drives {
        require_unique_positive(&mut drive_ids, drive.id, "drive.id")?;
        require_same_car(drive.car_id, selected_car_id, "drive.car_id")?;
        validate_interval(drive.start_date_ms, drive.end_date_ms, "drive")?;
        validate_optional_nonnegative(drive.distance_km, "drive.distance_km")?;
        validate_optional_finite(drive.efficiency, "drive.efficiency")?;
        validate_optional_finite(drive.power_max, "drive.power_max")?;
        validate_optional_finite(drive.power_min, "drive.power_min")?;
        validate_coordinate_pair(drive.start_latitude, drive.start_longitude, "drive.start")?;
        validate_coordinate_pair(drive.end_latitude, drive.end_longitude, "drive.end")?;
        validate_optional_soc(drive.start_soc, "drive.start_soc")?;
        validate_optional_soc(drive.end_soc, "drive.end_soc")?;
        for (value, name) in [
            (drive.start_address.as_deref(), "drive.start_address"),
            (drive.end_address.as_deref(), "drive.end_address"),
            (drive.start_geofence.as_deref(), "drive.start_geofence"),
            (drive.end_geofence.as_deref(), "drive.end_geofence"),
        ] {
            validate_optional_text(value, name)?;
        }
    }

    let mut charge_ids = HashSet::with_capacity(delta.charges.len());
    for charge in &delta.charges {
        require_unique_positive(&mut charge_ids, charge.id, "charge.id")?;
        require_same_car(charge.car_id, selected_car_id, "charge.car_id")?;
        require_positive(charge.start_date_ms, "charge.start_date_ms")?;
        if charge.end_date_ms.is_some_and(|end| end < charge.start_date_ms) {
            return Err(invalid("charge.end_date_ms precedes start_date_ms"));
        }
        validate_optional_nonnegative(charge.charge_energy_added, "charge.charge_energy_added")?;
        validate_optional_finite(charge.cost, "charge.cost")?;
        validate_coordinate_pair(charge.start_latitude, charge.start_longitude, "charge.start")?;
        validate_optional_soc(charge.start_battery_level, "charge.start_battery_level")?;
        validate_optional_soc(charge.end_battery_level, "charge.end_battery_level")?;
        for (value, name) in [
            (charge.address.as_deref(), "charge.address"),
            (charge.location_name.as_deref(), "charge.location_name"),
            (charge.geofence.as_deref(), "charge.geofence"),
        ] {
            validate_optional_text(value, name)?;
        }
    }

    let mut position_ids = HashSet::with_capacity(delta.positions.len());
    for position in &delta.positions {
        require_unique_positive(&mut position_ids, position.id, "position.id")?;
        require_same_car(position.car_id, selected_car_id, "position.car_id")?;
        require_positive(position.date_ms, "position.date_ms")?;
        if let Some(drive_id) = position.drive_id {
            require_positive(drive_id, "position.drive_id")?;
            // A missing parent is valid: it belongs to the declared external base.
            let _ = drive_ids.contains(&drive_id);
        }
        validate_coordinate(position.latitude, position.longitude, "position")?;
        validate_optional_soc(position.battery_level, "position.battery_level")?;
        validate_optional_soc(position.usable_battery_level, "position.usable_battery_level")?;
        validate_optional_finite(position.power, "position.power")?;
        validate_optional_nonnegative(position.odometer, "position.odometer")?;
        validate_optional_nonnegative(
            position.ideal_battery_range_km,
            "position.ideal_battery_range_km",
        )?;
    }

    let mut sample_ids = HashSet::with_capacity(delta.charge_samples.len());
    for sample in &delta.charge_samples {
        require_unique_positive(&mut sample_ids, sample.id, "charge_sample.id")?;
        require_positive(sample.timestamp_ms, "charge_sample.timestamp_ms")?;
        require_positive(sample.charge_process_id, "charge_sample.charge_process_id")?;
        let _ = charge_ids.contains(&sample.charge_process_id);
        validate_optional_soc(sample.battery_level, "charge_sample.battery_level")?;
        validate_optional_nonnegative(
            sample.charge_energy_added_kwh,
            "charge_sample.charge_energy_added_kwh",
        )?;
    }
    validate_states(&delta.states, selected_car_id)?;
    validate_updates(&delta.updates, selected_car_id)?;

    let mut tombstone_ids = HashSet::with_capacity(delta.tombstones.len());
    for tombstone in &delta.tombstones {
        require_positive(tombstone.id, "tombstone.id")?;
        require_same_car(tombstone.car_id, selected_car_id, "tombstone.car_id")?;
        if !tombstone_ids.insert((tombstone.entity, tombstone.id)) {
            return Err(invalid("duplicate typed tombstone"));
        }
    }
    let row_count = delta.row_count()?;
    if row_count == 0 || row_count > limits.max_rows_per_pack {
        return Err(ProjectionPackError::TooManyRows);
    }
    Ok(row_count)
}

fn tables_for_delta(delta: &ProjectionDelta) -> Vec<MirrorTable> {
    let mut tables = Vec::new();
    if !delta.cars.is_empty() || !delta.car_settings.is_empty() {
        tables.push(MirrorTable::Car);
    }
    if !delta.drives.is_empty() {
        tables.push(MirrorTable::Drive);
    }
    if !delta.charges.is_empty() {
        tables.push(MirrorTable::Charge);
    }
    if !delta.positions.is_empty() {
        tables.push(MirrorTable::Position);
    }
    if !delta.charge_samples.is_empty() {
        tables.push(MirrorTable::ChargeSample);
    }
    if !delta.states.is_empty() {
        tables.push(MirrorTable::State);
    }
    if !delta.updates.is_empty() {
        tables.push(MirrorTable::Update);
    }
    if !delta.tombstones.is_empty() {
        tables.push(MirrorTable::Tombstone);
    }
    tables
}

fn row_count_with_states_and_updates(
    snapshot: &ProjectionSnapshot,
    states: &[ProjectionState],
    updates: &[ProjectionUpdate],
) -> Result<u64, ProjectionPackError> {
    let with_states = snapshot
        .row_count()?
        .checked_add(u64::try_from(states.len()).map_err(|_| ProjectionPackError::TooManyRows)?)
        .ok_or(ProjectionPackError::TooManyRows)?;
    with_states
        .checked_add(u64::try_from(updates.len()).map_err(|_| ProjectionPackError::TooManyRows)?)
        .ok_or(ProjectionPackError::TooManyRows)
}

fn validate_states(
    states: &[ProjectionState],
    selected_car_id: i64,
) -> Result<(), ProjectionPackError> {
    let mut ids = HashSet::with_capacity(states.len());
    let mut open_cars = HashSet::new();
    for state in states {
        require_unique_positive(&mut ids, state.id, "state.id")?;
        require_same_car(state.car_id, selected_car_id, "state.car_id")?;
        if !matches!(state.state.as_str(), "online" | "offline" | "asleep") {
            return Err(invalid("state.state is not a TeslaMate state"));
        }
        require_positive(state.start_date_ms, "state.start_date_ms")?;
        if let Some(end) = state.end_date_ms {
            if end < state.start_date_ms {
                return Err(invalid("state.end_date_ms precedes state.start_date_ms"));
            }
        } else if !open_cars.insert(state.car_id) {
            return Err(invalid("more than one open state exists for a car"));
        }
    }
    Ok(())
}

fn validate_updates(
    updates: &[ProjectionUpdate],
    selected_car_id: i64,
) -> Result<(), ProjectionPackError> {
    let mut ids = HashSet::with_capacity(updates.len());
    for update in updates {
        require_unique_positive(&mut ids, update.id, "update.id")?;
        require_same_car(update.car_id, selected_car_id, "update.car_id")?;
        require_positive(update.start_date_ms, "update.start_date_ms")?;
        if update.end_date_ms < update.start_date_ms {
            return Err(invalid("update.end_date_ms precedes update.start_date_ms"));
        }
        validate_optional_text(Some(&update.version), "update.version")?;
    }
    Ok(())
}

fn validate_binding(binding: &ProjectionBinding) -> Result<(), ProjectionPackError> {
    if binding.installation_id.is_nil()
        || binding.account_id.is_nil()
        || binding.vehicle_id.is_nil()
        || binding.generation == 0
    {
        return Err(invalid("projection binding is incomplete"));
    }
    require_positive(binding.selected_car_id, "selected_car_id")
}

fn tables_for_snapshot(snapshot: &ProjectionSnapshot, includes_states: bool) -> Vec<MirrorTable> {
    let mut tables = vec![MirrorTable::Car];
    if !snapshot.drives.is_empty() {
        tables.push(MirrorTable::Drive);
    }
    if !snapshot.charges.is_empty() {
        tables.push(MirrorTable::Charge);
    }
    if !snapshot.positions.is_empty() {
        tables.push(MirrorTable::Position);
    }
    if !snapshot.charge_samples.is_empty() {
        tables.push(MirrorTable::ChargeSample);
    }
    // Schema 2.1 writes the state/update pair together. Advertise both
    // tables whenever that extension is present so consumers can discover
    // every table emitted by the pack without inferring it from SQLite.
    if includes_states {
        tables.push(MirrorTable::State);
        tables.push(MirrorTable::Update);
    }
    tables
}

fn write_projection_sqlite(
    path: &Path,
    request: &ProjectionPackRequest<'_>,
    limits: ProtocolLimits,
    schema: SchemaVersion,
    states: &[ProjectionState],
    updates: &[ProjectionUpdate],
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(ProjectionPackError::OpenSqlite)?;
    let max_pages = limits.max_uncompressed_pack_bytes / 4_096;
    if max_pages == 0 {
        return Err(invalid("pack limit is smaller than one SQLite page"));
    }
    connection
        .pragma_update(None, "page_size", 4_096_i64)
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    connection
        .pragma_update(
            None,
            "max_page_count",
            i64::try_from(max_pages).unwrap_or(i64::MAX),
        )
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    connection
        .execute_batch(
            "
            PRAGMA journal_mode = OFF;
            PRAGMA synchronous = OFF;
            PRAGMA foreign_keys = ON;
            PRAGMA trusted_schema = OFF;
            PRAGMA temp_store = FILE;
            BEGIN IMMEDIATE;
            CREATE TABLE hub_pack_metadata (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            ) STRICT;
            CREATE TABLE cars (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                model TEXT NOT NULL,
                vin TEXT,
                source_eid INTEGER,
                source_vid INTEGER,
                trim_badging TEXT,
                marketing_name TEXT,
                exterior_color TEXT,
                wheel_type TEXT,
                spoiler_type TEXT,
                firmware_version TEXT,
                efficiency_wh_per_km REAL
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE car_settings (
                car_id INTEGER PRIMARY KEY REFERENCES cars(id),
                enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
                use_streaming_api INTEGER NOT NULL CHECK(use_streaming_api IN (0, 1)),
                suspend_after_idle_min INTEGER NOT NULL CHECK(suspend_after_idle_min > 0),
                suspend_min INTEGER NOT NULL CHECK(suspend_min > 0),
                suspend_min_resolved INTEGER NOT NULL CHECK(suspend_min_resolved IN (0, 1)),
                req_not_unlocked INTEGER NOT NULL CHECK(req_not_unlocked IN (0, 1)),
                free_supercharging INTEGER NOT NULL CHECK(free_supercharging IN (0, 1)),
                lfp_battery INTEGER NOT NULL CHECK(lfp_battery IN (0, 1))
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE drives (
                id INTEGER PRIMARY KEY,
                car_id INTEGER NOT NULL REFERENCES cars(id),
                optimized_at_ms INTEGER,
                start_date_ms INTEGER NOT NULL,
                end_date_ms INTEGER NOT NULL,
                distance_km REAL,
                duration_min INTEGER,
                efficiency REAL,
                outside_temp_avg REAL,
                inside_temp_avg REAL,
                speed_max INTEGER,
                power_max REAL,
                power_min REAL,
                start_ideal_range_km REAL,
                end_ideal_range_km REAL,
                start_address TEXT,
                end_address TEXT,
                start_geofence TEXT,
                end_geofence TEXT,
                start_latitude REAL,
                start_longitude REAL,
                end_latitude REAL,
                end_longitude REAL,
                start_soc INTEGER,
                end_soc INTEGER,
                start_rated_range_km REAL,
                end_rated_range_km REAL,
                ascent INTEGER,
                descent INTEGER
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE charges (
                id INTEGER PRIMARY KEY,
                car_id INTEGER NOT NULL REFERENCES cars(id),
                start_date_ms INTEGER NOT NULL,
                end_date_ms INTEGER,
                charge_energy_added REAL,
                charge_energy_used_kwh REAL,
                start_ideal_range_km REAL,
                end_ideal_range_km REAL,
                cost REAL,
                fast_charger_type TEXT,
                billing_type TEXT CHECK (billing_type IS NULL OR billing_type IN ('per_kwh', 'per_minute')),
                cost_per_unit REAL,
                session_fee REAL,
                start_latitude REAL,
                start_longitude REAL,
                start_battery_level INTEGER,
                end_battery_level INTEGER,
                duration_min INTEGER,
                address TEXT,
                location_name TEXT,
                geofence TEXT,
                is_dc INTEGER CHECK (is_dc IN (0, 1)),
                charge_rate_km_per_hour REAL,
                max_charger_power_kw REAL,
                outside_temp_avg REAL,
                start_rated_range_km REAL,
                end_rated_range_km REAL
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE positions (
                id INTEGER PRIMARY KEY,
                drive_id INTEGER REFERENCES drives(id)
                    CHECK(drive_id IS NULL OR drive_id > 0),
                car_id INTEGER NOT NULL REFERENCES cars(id),
                date_ms INTEGER NOT NULL,
                latitude REAL NOT NULL,
                longitude REAL NOT NULL,
                speed INTEGER,
                power REAL,
                battery_level INTEGER,
                usable_battery_level INTEGER,
                elevation INTEGER,
                odometer REAL,
                ideal_battery_range_km REAL,
                est_battery_range_km REAL,
                rated_battery_range_km REAL,
                fan_status INTEGER,
                driver_temp_setting REAL,
                passenger_temp_setting REAL,
                is_climate_on INTEGER CHECK (is_climate_on IN (0, 1)),
                is_rear_defroster_on INTEGER CHECK (is_rear_defroster_on IN (0, 1)),
                is_front_defroster_on INTEGER CHECK (is_front_defroster_on IN (0, 1)),
                inside_temp REAL,
                outside_temp REAL,
                battery_heater INTEGER CHECK (battery_heater IN (0, 1)),
                battery_heater_on INTEGER CHECK (battery_heater_on IN (0, 1)),
                battery_heater_no_power INTEGER CHECK (battery_heater_no_power IN (0, 1)),
                tpms_pressure_fl REAL,
                tpms_pressure_fr REAL,
                tpms_pressure_rl REAL,
                tpms_pressure_rr REAL
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE charge_samples (
                id INTEGER PRIMARY KEY,
                charge_process_id INTEGER NOT NULL REFERENCES charges(id),
                timestamp_ms INTEGER NOT NULL,
                battery_level INTEGER,
                usable_battery_level INTEGER,
                charge_energy_added_kwh REAL,
                charger_power_kw REAL,
                charger_voltage REAL,
                charger_actual_current REAL,
                charger_pilot_current REAL,
                charger_phases INTEGER,
                ideal_range_km REAL,
                rated_range_km REAL,
                outside_temp_c REAL,
                battery_heater_on INTEGER CHECK (battery_heater_on IN (0, 1)),
                battery_heater INTEGER CHECK (battery_heater IN (0, 1)),
                battery_heater_no_power INTEGER CHECK (battery_heater_no_power IN (0, 1)),
                not_enough_power_to_heat INTEGER CHECK (not_enough_power_to_heat IN (0, 1)),
                fast_charger_present INTEGER CHECK (fast_charger_present IN (0, 1)),
                fast_charger_brand TEXT,
                fast_charger_type TEXT,
                charge_cable TEXT
            ) STRICT, WITHOUT ROWID;
            COMMIT;
            ",
        )
        .map_err(ProjectionPackError::CreateSchema)?;
    if schema == HUB_PROJECTION_SCHEMA_V2 {
        connection
            .execute_batch(
                "CREATE TABLE states (
                    id INTEGER PRIMARY KEY,
                    car_id INTEGER NOT NULL REFERENCES cars(id),
                    state TEXT NOT NULL CHECK (state IN ('online', 'offline', 'asleep')),
                    start_date_ms INTEGER NOT NULL,
                    end_date_ms INTEGER
                ) STRICT, WITHOUT ROWID;",
            )
            .map_err(ProjectionPackError::CreateSchema)?;
        connection
            .execute_batch(
                "CREATE TABLE updates (
                    id INTEGER PRIMARY KEY,
                    car_id INTEGER NOT NULL REFERENCES cars(id),
                    start_date_ms INTEGER NOT NULL,
                    end_date_ms INTEGER NOT NULL,
                    version TEXT NOT NULL
                ) STRICT, WITHOUT ROWID;",
            )
            .map_err(ProjectionPackError::CreateSchema)?;
    }

    let transaction = connection
        .unchecked_transaction()
        .map_err(ProjectionPackError::BeginTransaction)?;
    insert_metadata(&transaction, request, schema, row_count)?;
    insert_cars(&transaction, &request.snapshot.cars)?;
    insert_drives(&transaction, &request.snapshot.drives)?;
    insert_charges(&transaction, &request.snapshot.charges)?;
    insert_positions(&transaction, &request.snapshot.positions)?;
    insert_charge_samples(&transaction, &request.snapshot.charge_samples)?;
    if schema == HUB_PROJECTION_SCHEMA_V2 {
        insert_states(&transaction, states)?;
        insert_updates(&transaction, updates)?;
    }
    transaction.commit().map_err(ProjectionPackError::Commit)?;
    connection
        .execute_batch("PRAGMA optimize; VACUUM;")
        .map_err(ProjectionPackError::FinalizeSqlite)?;
    connection
        .pragma_update(None, "application_id", SQLITE_HUB_PROJECTION_APPLICATION_ID)
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    connection
        .pragma_update(None, "user_version", schema.sqlite_user_version())
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if integrity != "ok" {
        return Err(ProjectionPackError::IntegrityFailure);
    }
    Ok(())
}

fn insert_metadata(
    transaction: &rusqlite::Transaction<'_>,
    request: &ProjectionPackRequest<'_>,
    schema: SchemaVersion,
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let values = [
        ("protocol", "teslatlas-sync".to_owned()),
        ("pack_format", "hub_projection_sqlite".to_owned()),
        ("schema_major", schema.major.to_string()),
        ("schema_minor", schema.minor.to_string()),
        ("pack_id", request.pack_id.to_string()),
        ("snapshot_id", request.snapshot_id.to_string()),
        ("ordinal", request.ordinal.to_string()),
        ("mode", "full_snapshot".to_owned()),
        (
            "installation_id",
            request.binding.installation_id.to_string(),
        ),
        ("account_id", request.binding.account_id.to_string()),
        ("vehicle_id", request.binding.vehicle_id.to_string()),
        ("generation", request.binding.generation.to_string()),
        (
            "selected_car_id",
            request.binding.selected_car_id.to_string(),
        ),
        ("base_sequence", request.sequence.from_exclusive.to_string()),
        ("head_sequence", request.sequence.to_inclusive.to_string()),
        ("row_count", row_count.to_string()),
    ];
    let mut statement = transaction
        .prepare_cached("INSERT INTO hub_pack_metadata (key, value) VALUES (?1, ?2)")
        .map_err(ProjectionPackError::Prepare)?;
    for (key, value) in values {
        statement
            .execute(params![key, value])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn write_delta_rows(
    path: &Path,
    request: &ProjectionDeltaPackRequest<'_>,
    limits: ProtocolLimits,
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(ProjectionPackError::OpenSqlite)?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             PRAGMA synchronous = FULL;
             CREATE TABLE tombstones (
                 entity TEXT NOT NULL,
                 entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                 car_id INTEGER NOT NULL CHECK(car_id > 0),
                 PRIMARY KEY(entity, entity_id)
             ) STRICT, WITHOUT ROWID;",
        )
        .map_err(ProjectionPackError::CreateSchema)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(ProjectionPackError::BeginTransaction)?;
    transaction
        .execute("DELETE FROM hub_pack_metadata", [])
        .map_err(ProjectionPackError::Insert)?;
    insert_delta_metadata(&transaction, request, row_count)?;
    insert_cars(&transaction, &request.delta.cars)?;
    insert_car_settings(&transaction, &request.delta.car_settings)?;
    insert_drives(&transaction, &request.delta.drives)?;
    insert_charges(&transaction, &request.delta.charges)?;
    insert_positions(&transaction, &request.delta.positions)?;
    insert_charge_samples(&transaction, &request.delta.charge_samples)?;
    insert_states(&transaction, &request.delta.states)?;
    insert_updates(&transaction, &request.delta.updates)?;
    insert_tombstones(&transaction, &request.delta.tombstones)?;
    transaction.commit().map_err(ProjectionPackError::Commit)?;
    connection
        .execute_batch("PRAGMA optimize; VACUUM;")
        .map_err(ProjectionPackError::FinalizeSqlite)?;
    connection
        .pragma_update(None, "application_id", SQLITE_HUB_PROJECTION_APPLICATION_ID)
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    connection
        .pragma_update(None, "user_version", HUB_PROJECTION_SCHEMA_V2.sqlite_user_version())
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if integrity != "ok" {
        return Err(ProjectionPackError::IntegrityFailure);
    }
    let _ = limits;
    Ok(())
}

fn insert_delta_metadata(
    transaction: &rusqlite::Transaction<'_>,
    request: &ProjectionDeltaPackRequest<'_>,
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let delta = request.delta;
    let values = [
        ("protocol", "teslatlas-sync".to_owned()),
        ("pack_format", "hub_projection_sqlite".to_owned()),
        ("schema_major", HUB_PROJECTION_SCHEMA_V2.major.to_string()),
        ("schema_minor", HUB_PROJECTION_SCHEMA_V2.minor.to_string()),
        ("delta_schema_version", "1".to_owned()),
        ("pack_id", request.pack_id.to_string()),
        ("snapshot_id", request.snapshot_id.to_string()),
        ("ordinal", request.ordinal.to_string()),
        ("mode", "typed_delta".to_owned()),
        (
            "installation_id",
            delta.binding.installation_id.to_string(),
        ),
        ("account_id", delta.binding.account_id.to_string()),
        ("vehicle_id", delta.binding.vehicle_id.to_string()),
        ("generation", delta.binding.generation.to_string()),
        (
            "selected_car_id",
            delta.binding.selected_car_id.to_string(),
        ),
        ("from_sequence", delta.sequence.from_exclusive.to_string()),
        ("to_sequence", delta.sequence.to_inclusive.to_string()),
        ("parent_digest", delta.parent_digest.to_string()),
        ("external_base", "true".to_owned()),
        ("row_count", row_count.to_string()),
    ];
    let mut statement = transaction
        .prepare_cached("INSERT INTO hub_pack_metadata (key, value) VALUES (?1, ?2)")
        .map_err(ProjectionPackError::Prepare)?;
    for (key, value) in values {
        statement
            .execute(params![key, value])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_car_settings(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCarSettingsPatch],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.car_id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO car_settings(
                car_id, enabled, use_streaming_api, suspend_after_idle_min, suspend_min,
                suspend_min_resolved, req_not_unlocked, free_supercharging, lfp_battery
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.car_id,
                row.settings.enabled,
                row.settings.use_streaming_api,
                row.settings.suspend_after_idle_min,
                row.settings.suspend_min,
                row.settings.suspend_min_resolved,
                row.settings.req_not_unlocked,
                row.settings.free_supercharging,
                row.settings.lfp_battery,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_tombstones(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionTombstone],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| (row.entity.as_str(), row.id));
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO tombstones(entity, entity_id, car_id)
             VALUES (?1, ?2, ?3)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![row.entity.as_str(), row.id, row.car_id])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_states(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionState],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO states (id, car_id, state, start_date_ms, end_date_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.state,
                row.start_date_ms,
                row.end_date_ms,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_updates(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionUpdate],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO updates (id, car_id, start_date_ms, end_date_ms, version)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.start_date_ms,
                row.end_date_ms,
                row.version,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_cars(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCar],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO cars (
                id, name, model, vin, source_eid, source_vid, trim_badging,
                marketing_name, exterior_color, wheel_type, spoiler_type,
                firmware_version, efficiency_wh_per_km
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in &rows {
        let model = normalize_tesla_model_code(&row.model);
        statement
            .execute(params![
                row.id,
                row.name,
                model,
                row.vin,
                row.source_eid,
                row.source_vid,
                row.trim_badging,
                row.marketing_name,
                row.exterior_color,
                row.wheel_type,
                row.spoiler_type,
                row.firmware_version,
                row.efficiency_wh_per_km,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    let mut settings = transaction
        .prepare_cached(
            "INSERT INTO car_settings(
                car_id, enabled, use_streaming_api, suspend_after_idle_min, suspend_min,
                suspend_min_resolved,
                req_not_unlocked, free_supercharging, lfp_battery
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        settings
            .execute(params![
                row.id,
                row.settings.enabled,
                row.settings.use_streaming_api,
                row.settings.suspend_after_idle_min,
                row.settings.suspend_min,
                row.settings.suspend_min_resolved,
                row.settings.req_not_unlocked,
                row.settings.free_supercharging,
                row.settings.lfp_battery,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_drives(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionDrive],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO drives (
                id, car_id, optimized_at_ms, start_date_ms, end_date_ms, distance_km,
                duration_min, efficiency, outside_temp_avg, inside_temp_avg, speed_max,
                power_max, power_min, start_ideal_range_km, end_ideal_range_km, start_address,
                end_address, start_geofence, end_geofence, start_latitude, start_longitude,
                end_latitude, end_longitude, start_soc, end_soc, start_rated_range_km,
                end_rated_range_km, ascent, descent
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                ?27, ?28, ?29
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.optimized_at_ms,
                row.start_date_ms,
                row.end_date_ms,
                row.distance_km,
                row.duration_min,
                row.efficiency,
                row.outside_temp_avg,
                row.inside_temp_avg,
                row.speed_max,
                row.power_max,
                row.power_min,
                row.start_ideal_range_km,
                row.end_ideal_range_km,
                row.start_address,
                row.end_address,
                row.start_geofence,
                row.end_geofence,
                row.start_latitude,
                row.start_longitude,
                row.end_latitude,
                row.end_longitude,
                row.start_soc,
                row.end_soc,
                row.start_rated_range_km,
                row.end_rated_range_km,
                row.ascent,
                row.descent,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_charges(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCharge],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO charges (
                id, car_id, start_date_ms, end_date_ms, charge_energy_added,
                charge_energy_used_kwh, start_ideal_range_km, end_ideal_range_km,
                cost, fast_charger_type, billing_type, cost_per_unit, session_fee,
                start_latitude, start_longitude, start_battery_level,
                end_battery_level, duration_min, address, location_name, geofence,
                is_dc, charge_rate_km_per_hour, max_charger_power_kw,
                outside_temp_avg, start_rated_range_km, end_rated_range_km
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.start_date_ms,
                row.end_date_ms,
                row.charge_energy_added,
                row.charge_energy_used_kwh,
                row.start_ideal_range_km,
                row.end_ideal_range_km,
                row.cost,
                row.fast_charger_type,
                row.billing_type.map(GeofenceBillingType::as_str),
                row.cost_per_unit,
                row.session_fee,
                row.start_latitude,
                row.start_longitude,
                row.start_battery_level,
                row.end_battery_level,
                row.duration_min,
                row.address,
                row.location_name,
                row.geofence,
                bool_as_sql(row.is_dc),
                row.charge_rate_km_per_hour,
                row.max_charger_power_kw,
                row.outside_temp_avg,
                row.start_rated_range_km,
                row.end_rated_range_km,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_positions(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionPosition],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO positions (
                id, drive_id, car_id, date_ms, latitude, longitude, speed, power,
                battery_level, usable_battery_level, elevation, odometer,
                ideal_battery_range_km, est_battery_range_km, rated_battery_range_km,
                fan_status, driver_temp_setting, passenger_temp_setting, is_climate_on,
                is_rear_defroster_on, is_front_defroster_on, inside_temp, outside_temp,
                battery_heater, battery_heater_on, battery_heater_no_power,
                tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                ?27, ?28, ?29, ?30
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.drive_id,
                row.car_id,
                row.date_ms,
                row.latitude,
                row.longitude,
                row.speed,
                row.power,
                row.battery_level,
                row.usable_battery_level,
                row.elevation,
                row.odometer,
                row.ideal_battery_range_km,
                row.est_battery_range_km,
                row.rated_battery_range_km,
                row.fan_status,
                row.driver_temp_setting,
                row.passenger_temp_setting,
                bool_as_sql(row.is_climate_on),
                bool_as_sql(row.is_rear_defroster_on),
                bool_as_sql(row.is_front_defroster_on),
                row.inside_temp,
                row.outside_temp,
                bool_as_sql(row.battery_heater),
                bool_as_sql(row.battery_heater_on),
                bool_as_sql(row.battery_heater_no_power),
                row.tpms_pressure_fl,
                row.tpms_pressure_fr,
                row.tpms_pressure_rl,
                row.tpms_pressure_rr,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_charge_samples(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionChargeSample],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO charge_samples (
                id, charge_process_id, timestamp_ms, battery_level, usable_battery_level,
                charge_energy_added_kwh, charger_power_kw, charger_voltage,
                charger_actual_current, charger_pilot_current, charger_phases, ideal_range_km,
                rated_range_km, outside_temp_c, battery_heater_on, battery_heater,
                battery_heater_no_power, not_enough_power_to_heat, fast_charger_present,
                fast_charger_brand, fast_charger_type, charge_cable
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.charge_process_id,
                row.timestamp_ms,
                row.battery_level,
                row.usable_battery_level,
                row.charge_energy_added_kwh,
                row.charger_power_kw,
                row.charger_voltage,
                row.charger_actual_current,
                row.charger_pilot_current,
                row.charger_phases,
                row.ideal_range_km,
                row.rated_range_km,
                row.outside_temp_c,
                bool_as_sql(row.battery_heater_on),
                bool_as_sql(row.battery_heater),
                bool_as_sql(row.battery_heater_no_power),
                bool_as_sql(row.not_enough_power_to_heat),
                bool_as_sql(row.fast_charger_present),
                row.fast_charger_brand,
                row.fast_charger_type,
                row.charge_cable,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn bool_as_sql(value: Option<bool>) -> Option<i64> {
    value.map(i64::from)
}

fn verify_file(
    metadata: &TransportPack,
    path: &Path,
    limits: ProtocolLimits,
) -> Result<VerifiedTransportPack, ProjectionPackError> {
    let file = File::open(path).map_err(|source| ProjectionPackError::OpenCompressed {
        path: path.to_path_buf(),
        source,
    })?;
    metadata
        .verify_reader(file, limits)
        .map_err(ProjectionPackError::Protocol)
}

fn compress_file(
    source_path: &Path,
    destination_path: &Path,
) -> Result<(Sha256Digest, u64), ProjectionPackError> {
    let mut source = File::open(source_path).map_err(|source| ProjectionPackError::ReadSource {
        path: source_path.to_path_buf(),
        source,
    })?;
    let destination = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(destination_path)
        .map_err(|source| ProjectionPackError::CreateCompressed {
            path: destination_path.to_path_buf(),
            source,
        })?;
    let mut encoder =
        zstd::stream::write::Encoder::new(HashingWriter::new(destination), COMPRESSION_LEVEL)
            .map_err(ProjectionPackError::Compress)?;
    io::copy(&mut source, &mut encoder).map_err(ProjectionPackError::Compress)?;
    let (file, digest, bytes) = encoder
        .finish()
        .map_err(ProjectionPackError::Compress)?
        .finish();
    file.sync_all()
        .map_err(ProjectionPackError::SyncCompressed)?;
    Ok((digest, bytes))
}

fn available_bytes(path: &Path) -> Result<u64, ProjectionPackError> {
    let stats = statvfs(path).map_err(|source| ProjectionPackError::FilesystemSpace {
        path: path.to_path_buf(),
        source,
    })?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or(ProjectionPackError::CapacityOverflow)
}

fn publish_immutable(
    temporary_path: &Path,
    final_path: &Path,
    metadata: &TransportPack,
    limits: ProtocolLimits,
) -> Result<(), ProjectionPackError> {
    match fs::hard_link(temporary_path, final_path) {
        Ok(()) => sync_parent_directory(final_path),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            verify_file(metadata, final_path, limits).map(|_| ())
        }
        Err(source) => Err(ProjectionPackError::Publish {
            path: final_path.to_path_buf(),
            source,
        }),
    }
}

fn sync_parent_directory(path: &Path) -> Result<(), ProjectionPackError> {
    let parent = path.parent().ok_or_else(|| ProjectionPackError::Publish {
        path: path.to_path_buf(),
        source: io::Error::other("immutable pack has no parent directory"),
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ProjectionPackError::Publish {
            path: path.to_path_buf(),
            source,
        })
}

fn invalid(message: impl Into<String>) -> ProjectionPackError {
    ProjectionPackError::Invalid(message.into())
}

fn require_positive(value: i64, field: &str) -> Result<(), ProjectionPackError> {
    if value <= 0 {
        return Err(invalid(format!("{field} must be positive")));
    }
    Ok(())
}

fn require_unique_positive(
    ids: &mut HashSet<i64>,
    value: i64,
    field: &str,
) -> Result<(), ProjectionPackError> {
    require_positive(value, field)?;
    if !ids.insert(value) {
        return Err(invalid(format!("duplicate {field}")));
    }
    Ok(())
}

fn require_same_car(value: i64, expected: i64, field: &str) -> Result<(), ProjectionPackError> {
    if value != expected {
        return Err(invalid(format!("{field} does not match selected_car_id")));
    }
    Ok(())
}

fn validate_interval(start: i64, end: i64, field: &str) -> Result<(), ProjectionPackError> {
    require_positive(start, &format!("{field}.start_date_ms"))?;
    require_positive(end, &format!("{field}.end_date_ms"))?;
    if end < start {
        return Err(invalid(format!(
            "{field}.end_date_ms precedes start_date_ms"
        )));
    }
    Ok(())
}

fn validate_optional_positive(value: Option<i64>, field: &str) -> Result<(), ProjectionPackError> {
    if let Some(value) = value {
        require_positive(value, field)?;
    }
    Ok(())
}

fn validate_optional_nonnegative(
    value: Option<f64>,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return Err(invalid(format!("{field} must be finite and nonnegative")));
    }
    Ok(())
}

fn validate_optional_finite(value: Option<f64>, field: &str) -> Result<(), ProjectionPackError> {
    if value.is_some_and(|value| !value.is_finite()) {
        return Err(invalid(format!("{field} must be finite")));
    }
    Ok(())
}

fn validate_optional_soc(value: Option<i64>, field: &str) -> Result<(), ProjectionPackError> {
    if value.is_some_and(|value| !(0..=100).contains(&value)) {
        return Err(invalid(format!("{field} must be between 0 and 100")));
    }
    Ok(())
}

fn validate_coordinate_pair(
    latitude: Option<f64>,
    longitude: Option<f64>,
    field: &str,
) -> Result<(), ProjectionPackError> {
    match (latitude, longitude) {
        (None, None) => Ok(()),
        (Some(latitude), Some(longitude)) => validate_coordinate(latitude, longitude, field),
        _ => Err(invalid(format!("{field} coordinate pair is incomplete"))),
    }
}

fn validate_coordinate(
    latitude: f64,
    longitude: f64,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if !latitude.is_finite()
        || !longitude.is_finite()
        || !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
        || (latitude == 0.0 && longitude == 0.0)
    {
        return Err(invalid(format!("{field} coordinates are invalid")));
    }
    Ok(())
}

fn validate_required_text(value: &str, field: &str) -> Result<(), ProjectionPackError> {
    if value.is_empty() {
        return Err(invalid(format!("{field} must not be empty")));
    }
    validate_optional_text(Some(value), field)
}

fn validate_optional_text(value: Option<&str>, field: &str) -> Result<(), ProjectionPackError> {
    if value.is_some_and(|value| value.len() > MAX_TEXT_BYTES || value.as_bytes().contains(&0)) {
        return Err(invalid(format!("{field} is unsafe or too large")));
    }
    Ok(())
}

struct StagedFile {
    path: PathBuf,
}

impl StagedFile {
    fn create(directory: &Path, extension: &str) -> Result<Self, ProjectionPackError> {
        for _ in 0..32 {
            let path = directory.join(format!("{}.{}.tmp", Uuid::new_v4(), extension));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => {
                    drop(file);
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(ProjectionPackError::CreateTemporary { path, source }),
            }
        }
        Err(ProjectionPackError::CreateTemporary {
            path: directory.join(format!("exhausted.{extension}.tmp")),
            source: io::Error::new(io::ErrorKind::AlreadyExists, "temporary name collision"),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    bytes_written: u64,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes_written: 0,
        }
    }

    fn finish(self) -> (W, Sha256Digest, u64) {
        (
            self.inner,
            Sha256Digest::from_bytes(self.hasher.finalize().into()),
            self.bytes_written,
        )
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.bytes_written += u64::try_from(written).expect("usize fits into u64");
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Debug, Error)]
pub enum ProjectionPackError {
    #[error("invalid Hub projection pack: {0}")]
    Invalid(String),
    #[error("projection pack exceeds the configured row limit")]
    TooManyRows,
    #[error("projection snapshot has too many chunks")]
    TooManyChunks,
    #[error("projection snapshot totals overflow")]
    ManifestTotalsOverflow,
    #[error("projection pack capacity calculation overflowed")]
    CapacityOverflow,
    #[error("could not inspect free space for projection packs at {path}: {source}")]
    FilesystemSpace {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error(
        "projection full snapshot needs {required} free bytes but only {available} are available"
    )]
    InsufficientFreeSpace { required: u64, available: u64 },
    #[error("cannot create pack directory {path}: {source}")]
    CreateDirectory { path: PathBuf, source: io::Error },
    #[error("cannot create temporary projection pack {path}: {source}")]
    CreateTemporary { path: PathBuf, source: io::Error },
    #[error("cannot inspect temporary projection pack {path}: {source}")]
    Metadata { path: PathBuf, source: io::Error },
    #[error("cannot open projection SQLite pack: {0}")]
    OpenSqlite(rusqlite::Error),
    #[error("cannot configure projection SQLite pack: {0}")]
    ConfigureSqlite(rusqlite::Error),
    #[error("cannot create projection SQLite schema: {0}")]
    CreateSchema(rusqlite::Error),
    #[error("cannot begin projection SQLite transaction: {0}")]
    BeginTransaction(rusqlite::Error),
    #[error("cannot prepare projection insert: {0}")]
    Prepare(rusqlite::Error),
    #[error("cannot insert projection row: {0}")]
    Insert(rusqlite::Error),
    #[error("cannot commit projection SQLite transaction: {0}")]
    Commit(rusqlite::Error),
    #[error("cannot finalise projection SQLite pack: {0}")]
    FinalizeSqlite(rusqlite::Error),
    #[error("projection SQLite integrity check failed to run: {0}")]
    IntegrityCheck(rusqlite::Error),
    #[error("projection SQLite integrity check failed")]
    IntegrityFailure,
    #[error("cannot read projection SQLite source {path}: {source}")]
    ReadSource { path: PathBuf, source: io::Error },
    #[error("cannot create compressed projection pack {path}: {source}")]
    CreateCompressed { path: PathBuf, source: io::Error },
    #[error("cannot compress projection pack: {0}")]
    Compress(io::Error),
    #[error("cannot synchronise compressed projection pack: {0}")]
    SyncCompressed(io::Error),
    #[error("cannot open compressed projection pack {path}: {source}")]
    OpenCompressed { path: PathBuf, source: io::Error },
    #[error("cannot publish immutable projection pack {path}: {source}")]
    Publish { path: PathBuf, source: io::Error },
    #[error("projection protocol validation failed: {0}")]
    Protocol(#[from] ProtocolError),
}

#[cfg(test)]
mod tests {
    use std::env;

    use crate::protocol::{
        CursorClaims, LineageBase, LineageCapability, LineageDelta, LineageManifestV2,
        OpaqueCursor, PROTOCOL_V1, LINEAGE_PROTOCOL_V2,
    };

    use super::*;

    #[test]
    fn owner_api_model_codes_are_normalized_like_teslamate() {
        assert_eq!(normalize_tesla_model_code("model3"), "3");
        assert_eq!(normalize_tesla_model_code("models2"), "S");
        assert_eq!(normalize_tesla_model_code("modely"), "Y");
        assert_eq!(normalize_tesla_model_code("Model 3"), "3");
    }

    #[test]
    fn teslamate_suspend_default_matches_creation_conditions() {
        assert_eq!(teslamate_suspend_min_default(Some("3"), Some("74D"), None), Some(12));
        assert_eq!(teslamate_suspend_min_default(Some("Y"), None, None), Some(12));
        assert_eq!(teslamate_suspend_min_default(Some("S"), None, None), Some(12));
        assert_eq!(teslamate_suspend_min_default(Some("X"), Some("100D"), Some("LR")), Some(12));
        assert_eq!(teslamate_suspend_min_default(Some("S"), Some("100D"), None), Some(21));
        assert_eq!(teslamate_suspend_min_default(Some("Cybertruck"), None, None), None);
    }

    fn binding() -> ProjectionBinding {
        ProjectionBinding {
            installation_id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
            account_id: Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
            vehicle_id: Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap(),
            generation: 1,
            selected_car_id: 10,
        }
    }

    fn snapshot() -> ProjectionSnapshot {
        ProjectionSnapshot {
            cars: vec![ProjectionCar {
                id: 10,
                name: "Road car".into(),
                model: "Model 3".into(),
                vin: Some("5YJTESTVIN1234567".into()),
                source_eid: Some(101),
                source_vid: Some(201),
                trim_badging: Some("74D".into()),
                marketing_name: Some("LR AWD".into()),
                exterior_color: Some("Pearl White".into()),
                wheel_type: Some("Apollo".into()),
                spoiler_type: Some("None".into()),
                firmware_version: Some("2026.1.1".into()),
                efficiency_wh_per_km: Some(145.0),
                settings: ProjectionCarSettings::default(),
            }],
            drives: vec![ProjectionDrive {
                id: 20,
                car_id: 10,
                optimized_at_ms: None,
                start_date_ms: 1_700_000_000_000,
                end_date_ms: 1_700_000_060_000,
                distance_km: Some(12.5),
                duration_min: Some(10),
                efficiency: Some(145.0),
                outside_temp_avg: Some(18.5),
                inside_temp_avg: Some(20.0),
                speed_max: Some(80),
                power_max: Some(36.0),
                power_min: Some(-7.0),
                start_ideal_range_km: Some(390.0),
                end_ideal_range_km: Some(385.0),
                start_address: Some("Home".into()),
                end_address: Some("Work".into()),
                start_geofence: None,
                end_geofence: None,
                start_latitude: Some(51.5),
                start_longitude: Some(-0.1),
                end_latitude: Some(51.51),
                end_longitude: Some(-0.11),
                start_soc: Some(80),
                end_soc: Some(75),
                start_rated_range_km: Some(400.0),
                end_rated_range_km: Some(375.0),
                ascent: Some(60),
                descent: Some(30),
            }],
            positions: vec![ProjectionPosition {
                id: 30,
                drive_id: Some(20),
                car_id: 10,
                date_ms: 1_700_000_030_000,
                latitude: 51.505,
                longitude: -0.105,
                speed: Some(40),
                power: Some(3.0),
                battery_level: Some(78),
                usable_battery_level: Some(77),
                elevation: Some(25),
                odometer: Some(10_000.5),
                ideal_battery_range_km: Some(390.0),
                est_battery_range_km: Some(385.0),
                rated_battery_range_km: Some(388.0),
                fan_status: Some(2),
                driver_temp_setting: Some(21.5),
                passenger_temp_setting: Some(22.0),
                is_climate_on: Some(false),
                is_rear_defroster_on: Some(false),
                is_front_defroster_on: Some(true),
                inside_temp: Some(20.0),
                outside_temp: Some(18.0),
                battery_heater: None,
                battery_heater_on: None,
                battery_heater_no_power: None,
                tpms_pressure_fl: Some(2.4),
                tpms_pressure_fr: Some(2.5),
                tpms_pressure_rl: Some(2.6),
                tpms_pressure_rr: Some(2.7),
            }],
            charges: vec![ProjectionCharge {
                id: 40,
                car_id: 10,
                start_date_ms: 1_700_001_000_000,
                end_date_ms: Some(1_700_001_360_000),
                charge_energy_added: Some(20.0),
                charge_energy_used_kwh: None,
                start_ideal_range_km: None,
                end_ideal_range_km: None,
                cost: None,
                fast_charger_type: None,
                billing_type: None,
                cost_per_unit: None,
                session_fee: None,
                start_latitude: None,
                start_longitude: None,
                start_battery_level: Some(50),
                end_battery_level: Some(80),
                duration_min: Some(60),
                address: Some("Home".into()),
                location_name: None,
                geofence: None,
                is_dc: Some(false),
                charge_rate_km_per_hour: Some(40.0),
                max_charger_power_kw: Some(7.0),
                outside_temp_avg: Some(18.0),
                start_rated_range_km: Some(250.0),
                end_rated_range_km: Some(400.0),
            }],
            charge_samples: vec![ProjectionChargeSample {
                id: 50,
                charge_process_id: 40,
                timestamp_ms: 1_700_001_100_000,
                battery_level: Some(60),
                usable_battery_level: Some(59),
                charge_energy_added_kwh: Some(6.0),
                charger_power_kw: Some(7.0),
                charger_voltage: Some(230.0),
                charger_actual_current: Some(30.0),
                charger_pilot_current: Some(32.0),
                charger_phases: Some(1),
                ideal_range_km: Some(300.0),
                rated_range_km: Some(298.0),
                outside_temp_c: Some(18.0),
                battery_heater_on: Some(false),
                battery_heater: Some(false),
                battery_heater_no_power: Some(false),
                not_enough_power_to_heat: Some(false),
                fast_charger_present: Some(false),
                fast_charger_brand: None,
                fast_charger_type: None,
                charge_cable: Some("Type 2".into()),
            }],
        }
    }

    fn request<'a>(snapshot: &'a ProjectionSnapshot) -> ProjectionPackRequest<'a> {
        ProjectionPackRequest {
            pack_id: Uuid::parse_str("44444444-4444-4444-8444-444444444444").unwrap(),
            snapshot_id: Uuid::parse_str("55555555-5555-4555-8555-555555555555").unwrap(),
            ordinal: 0,
            binding: binding(),
            sequence: SequenceRange {
                from_exclusive: 7,
                to_inclusive: 7,
            },
            snapshot,
        }
    }

    #[test]
    fn writes_a_checked_typed_projection_pack() {
        let temporary = tempfile::tempdir().unwrap();
        let source = snapshot();
        let built = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot(&request(&source))
            .unwrap();
        assert_eq!(built.metadata.schema, HUB_PROJECTION_SCHEMA_V1);
        assert_eq!(built.metadata.format, PackFormat::HubProjectionSqlite);
        assert_eq!(built.metadata.row_count, 5);
        built
            .metadata
            .verify_reader(File::open(&built.path).unwrap(), ProtocolLimits::default())
            .unwrap();

        let sqlite = zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap();
        let inspect = temporary.path().join("inspect.sqlite");
        fs::write(&inspect, sqlite).unwrap();
        let connection = Connection::open(inspect).unwrap();
        let application_id: u32 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .unwrap();
        assert_eq!(application_id, SQLITE_HUB_PROJECTION_APPLICATION_ID);
        for table in ["cars", "drives", "positions", "charges", "charge_samples"] {
            let rows: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(rows, 1, "{table}");
        }
        let selected_car_id: String = connection
            .query_row(
                "SELECT value FROM hub_pack_metadata WHERE key = 'selected_car_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(selected_car_id, "10");
    }

    #[test]
    fn signs_and_catalogues_a_typed_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let source = snapshot();
        let request = request(&source);
        let built = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot(&request)
            .unwrap();
        let key = CursorKey::from_bytes([9; 32]);
        let manifest = request.signed_manifest(&built, &key).unwrap();
        manifest.validate_terminal_cursor(&key).unwrap();

        let store = crate::db::HubStore::initialize(temporary.path()).unwrap();
        store.publish_manifest(&manifest).unwrap();
        assert_eq!(
            store
                .manifest_for_vehicle(request.binding.vehicle_id)
                .unwrap()
                .unwrap(),
            manifest
        );
        assert_eq!(
            store
                .pack_for_digest(built.metadata.sha256)
                .unwrap()
                .unwrap()
                .path,
            built.path
        );
    }

    #[test]
    fn signs_several_parent_complete_snapshot_chunks() {
        let temporary = tempfile::tempdir().unwrap();
        let source = snapshot();
        let first_request = request(&source);
        let first = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot(&first_request)
            .unwrap();

        let mut second_snapshot = snapshot();
        second_snapshot.positions.clear();
        second_snapshot.charge_samples.clear();
        let mut second_request = request(&second_snapshot);
        second_request.pack_id = Uuid::new_v4();
        second_request.ordinal = 1;
        let second = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot(&second_request)
            .unwrap();

        let key = CursorKey::from_bytes([3; 32]);
        let manifest = signed_full_snapshot_manifest(
            &first_request.binding,
            first_request.snapshot_id,
            first_request.sequence,
            &[first.clone(), second.clone()],
            first_request.snapshot.row_count().unwrap(),
            &key,
        )
        .unwrap();
        assert_eq!(manifest.chunk_count, 2);
        assert_eq!(manifest.chunks[0].ordinal, 0);
        assert_eq!(manifest.chunks[1].ordinal, 1);
        assert_eq!(manifest.total_rows, first.metadata.row_count);
        manifest.validate_terminal_cursor(&key).unwrap();
    }

    #[test]
    fn rejects_a_position_without_its_drive_or_valid_coordinates() {
        let mut source = snapshot();
        source.positions[0].drive_id = Some(999);
        let temporary = tempfile::tempdir().unwrap();
        assert!(matches!(
            ProjectionPackWriter::new(temporary.path().join("packs"))
                .write_full_snapshot(&request(&source)),
            Err(ProjectionPackError::Invalid(_))
        ));

        let mut source = snapshot();
        source.positions[0].latitude = 0.0;
        source.positions[0].longitude = 0.0;
        assert!(matches!(
            ProjectionPackWriter::new(temporary.path().join("packs"))
                .write_full_snapshot(&request(&source)),
            Err(ProjectionPackError::Invalid(_))
        ));
    }

    #[test]
    fn schema_2_1_state_pack_preserves_ordered_rows_and_open_end_date() {
        let temporary = tempfile::tempdir().unwrap();
        let source = snapshot();
        let states = vec![
            ProjectionState {
                id: 12,
                car_id: 10,
                state: "asleep".into(),
                start_date_ms: 1_700_000_200_000,
                end_date_ms: Some(1_700_000_300_000),
            },
            ProjectionState {
                id: 11,
                car_id: 10,
                state: "online".into(),
                start_date_ms: 1_700_000_100_000,
                end_date_ms: None,
            },
        ];
        let updates = vec![ProjectionUpdate {
            id: 21,
            car_id: 10,
            start_date_ms: 1_700_000_400_000,
            end_date_ms: 1_700_000_500_000,
            version: "2026.2".into(),
        }];
        let built = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot_with_states_and_updates(&request(&source), &states, &updates)
            .unwrap();
        assert_eq!(built.metadata.schema, HUB_PROJECTION_SCHEMA_V2);

        let sqlite = zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap();
        let inspect = temporary.path().join("inspect.sqlite");
        fs::write(&inspect, sqlite).unwrap();
        let connection = Connection::open(inspect).unwrap();
        let rows: Vec<(i64, String, Option<i64>)> = connection
            .prepare("SELECT id, state, end_date_ms FROM states ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![
                (11, "online".into(), None),
                (12, "asleep".into(), Some(1_700_000_300_000)),
            ]
        );
        let update: (i64, String) = connection
            .query_row("SELECT id, version FROM updates", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(update, (21, "2026.2".into()));
    }

    #[test]
    fn schema_2_0_pack_has_no_states_table() {
        let temporary = tempfile::tempdir().unwrap();
        let source = snapshot();
        let built = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot(&request(&source))
            .unwrap();
        assert_eq!(built.metadata.schema, HUB_PROJECTION_SCHEMA_V1);

        let sqlite = zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap();
        let inspect = temporary.path().join("inspect.sqlite");
        fs::write(&inspect, sqlite).unwrap();
        let connection = Connection::open(inspect).unwrap();
        let states_table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'states'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(states_table_count, 0);
    }

    fn delta_request<'a>(delta: &'a ProjectionDelta) -> ProjectionDeltaPackRequest<'a> {
        ProjectionDeltaPackRequest {
            pack_id: Uuid::parse_str("66666666-6666-4666-8666-666666666666").unwrap(),
            snapshot_id: Uuid::parse_str("77777777-7777-4777-8777-777777777777").unwrap(),
            ordinal: 0,
            delta,
        }
    }

    fn sparse_delta() -> ProjectionDelta {
        let source = snapshot();
        let mut drive = source.drives[0].clone();
        drive.end_date_ms += 60_000;
        drive.end_address = Some("New work address".into());
        let mut position = source.positions[0].clone();
        position.id = 31;
        position.date_ms += 60_000;
        let mut car = source.cars[0].clone();
        car.name = "Road car renamed".into();
        ProjectionDelta {
            binding: binding(),
            sequence: SequenceRange {
                from_exclusive: 7,
                to_inclusive: 8,
            },
            parent_digest: Sha256Digest::of_bytes(b"base-lineage"),
            cars: vec![car],
            car_settings: Vec::new(),
            drives: vec![drive],
            positions: vec![position],
            charges: Vec::new(),
            charge_samples: Vec::new(),
            states: vec![ProjectionState {
                id: 60,
                car_id: 10,
                state: "online".into(),
                start_date_ms: 1_700_002_000_000,
                end_date_ms: None,
            }],
            updates: vec![ProjectionUpdate {
                id: 70,
                car_id: 10,
                start_date_ms: 1_700_002_100_000,
                end_date_ms: 1_700_002_200_000,
                version: "2026.3".into(),
            }],
            tombstones: vec![ProjectionTombstone {
                entity: ProjectionDeltaEntity::Position,
                id: 29,
                car_id: 10,
            }],
        }
    }

    #[test]
    fn writes_sparse_schema_2_1_delta_without_base_copy() {
        let temporary = tempfile::tempdir().unwrap();
        let delta = sparse_delta();
        let built = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_delta(&delta_request(&delta))
            .unwrap();
        assert_eq!(built.metadata.schema, HUB_PROJECTION_SCHEMA_V2);
        assert_eq!(built.metadata.sequence.from_exclusive, 7);
        assert_eq!(built.metadata.sequence.to_inclusive, 8);
        assert_eq!(built.metadata.row_count, 6);
        assert!(built.metadata.tables.contains(&MirrorTable::Tombstone));

        let sqlite = zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap();
        let inspect = temporary.path().join("inspect.sqlite");
        fs::write(&inspect, sqlite).unwrap();
        let connection = Connection::open(inspect).unwrap();
        let mode: String = connection
            .query_row("SELECT value FROM hub_pack_metadata WHERE key = 'mode'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(mode, "typed_delta");
        let parent: String = connection
            .query_row(
                "SELECT value FROM hub_pack_metadata WHERE key = 'parent_digest'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(parent, Sha256Digest::of_bytes(b"base-lineage").to_string());
        let positions: i64 = connection
            .query_row("SELECT COUNT(*) FROM positions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(positions, 1);
        let tombstone: (String, i64, i64) = connection
            .query_row("SELECT entity, entity_id, car_id FROM tombstones", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(tombstone, ("position".into(), 29, 10));
    }

    #[test]
    fn delta_output_is_deterministic_and_rejects_bad_binding_or_parent() {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let delta = sparse_delta();
        let first = ProjectionPackWriter::new(first_dir.path().join("packs"))
            .write_delta(&delta_request(&delta))
            .unwrap();
        let second = ProjectionPackWriter::new(second_dir.path().join("packs"))
            .write_delta(&delta_request(&delta))
            .unwrap();
        assert_eq!(
            fs::read(first.path).unwrap(),
            fs::read(second.path).unwrap()
        );
        assert_eq!(first.metadata.sha256, second.metadata.sha256);

        let mut bad_parent = delta.clone();
        bad_parent.parent_digest = Sha256Digest::from_bytes([0; 32]);
        assert!(matches!(
            ProjectionPackWriter::new(first_dir.path().join("bad-parent"))
                .write_delta(&delta_request(&bad_parent)),
            Err(ProjectionPackError::Invalid(_))
        ));

        let mut bad_binding = delta;
        bad_binding.positions[0].car_id = 99;
        assert!(matches!(
            ProjectionPackWriter::new(first_dir.path().join("bad-binding"))
                .write_delta(&delta_request(&bad_binding)),
            Err(ProjectionPackError::Invalid(_))
        ));
    }

    fn fixture_delta_request<'a>(
        delta: &'a ProjectionDelta,
        pack_id: &str,
        snapshot_id: &str,
    ) -> ProjectionDeltaPackRequest<'a> {
        ProjectionDeltaPackRequest {
            pack_id: Uuid::parse_str(pack_id).unwrap(),
            snapshot_id: Uuid::parse_str(snapshot_id).unwrap(),
            ordinal: 0,
            delta,
        }
    }

    fn fixture_lineage(root: &Path) -> (LineageManifestV2, Vec<(String, Vec<u8>)>) {
        let build_root = root.join("build");
        let writer = ProjectionPackWriter::new(build_root.join("packs"));
        let source = snapshot();
        let base_request = request(&source);
        let base = writer
            .write_full_snapshot_with_states_and_updates(
                &base_request,
                &[ProjectionState {
                    id: 11,
                    car_id: 10,
                    state: "online".into(),
                    start_date_ms: 1_700_000_000_000,
                    end_date_ms: None,
                }],
                &[],
            )
            .unwrap();

        let mut open_drive = source.drives[0].clone();
        open_drive.end_date_ms = 1_700_000_060_000;
        let mut new_position = source.positions[0].clone();
        new_position.id = 31;
        new_position.date_ms = 1_700_000_090_000;
        let first_delta = ProjectionDelta {
            binding: binding(),
            sequence: SequenceRange {
                from_exclusive: 7,
                to_inclusive: 8,
            },
            parent_digest: base.metadata.sha256,
            cars: vec![],
            car_settings: vec![],
            drives: vec![open_drive],
            positions: vec![new_position],
            charges: vec![],
            charge_samples: vec![],
            states: vec![],
            updates: vec![],
            tombstones: vec![],
        };
        let first = writer
            .write_delta(&fixture_delta_request(
                &first_delta,
                "88888888-8888-4888-8888-888888888881",
                "88888888-8888-4888-8888-888888888882",
            ))
            .unwrap();

        let mut closed_drive = source.drives[0].clone();
        closed_drive.end_date_ms = 1_700_000_120_000;
        let sparse_car = ProjectionCar {
            id: 10,
            name: "Road car renamed".into(),
            model: "Model 3".into(),
            vin: None,
            source_eid: None,
            source_vid: None,
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            firmware_version: None,
            efficiency_wh_per_km: None,
            settings: ProjectionCarSettings::default(),
        };
        let second_delta = ProjectionDelta {
            binding: binding(),
            sequence: SequenceRange {
                from_exclusive: 8,
                to_inclusive: 9,
            },
            parent_digest: first.metadata.sha256,
            cars: vec![sparse_car],
            car_settings: vec![],
            drives: vec![closed_drive],
            positions: vec![],
            charges: vec![],
            charge_samples: vec![],
            states: vec![],
            updates: vec![],
            tombstones: vec![ProjectionTombstone {
                entity: ProjectionDeltaEntity::Position,
                id: 30,
                car_id: 10,
            }],
        };
        let second = writer
            .write_delta(&fixture_delta_request(
                &second_delta,
                "99999999-9999-4999-8999-999999999991",
                "99999999-9999-4999-8999-999999999992",
            ))
            .unwrap();

        let key = CursorKey::from_bytes([42; 32]);
        let chain_one = Sha256Digest::of_bytes(
            format!("delta-v2/{}:{}", base.metadata.sha256, first.metadata.sha256).as_bytes(),
        );
        let chain_two = Sha256Digest::of_bytes(
            format!("delta-v2/{}:{}", chain_one, second.metadata.sha256).as_bytes(),
        );
        let terminal_cursor = OpaqueCursor::issue(
            &key,
            CursorClaims {
                protocol: PROTOCOL_V1,
                schema: HUB_PROJECTION_SCHEMA_V2,
                installation_id: binding().installation_id,
                account_id: binding().account_id,
                vehicle_id: binding().vehicle_id,
                generation: binding().generation,
                sequence: 9,
            },
        )
        .unwrap();
        let manifest = LineageManifestV2 {
            protocol: LINEAGE_PROTOCOL_V2,
            capability: LineageCapability::ImmutableBaseOrderedDeltas,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding().installation_id,
            account_id: binding().account_id,
            vehicle_id: binding().vehicle_id,
            generation: 1,
            base: LineageBase {
                snapshot_id: base.metadata.snapshot_id,
                sequence: 7,
                digest: base.metadata.sha256,
                packs: vec![base.metadata.clone()],
            },
            deltas: vec![
                LineageDelta {
                    from_sequence: 7,
                    to_sequence: 8,
                    parent_chain_digest: base.metadata.sha256,
                    chain_digest: chain_one,
                    pack_digest: first.metadata.sha256,
                    pack: first.metadata.clone(),
                },
                LineageDelta {
                    from_sequence: 8,
                    to_sequence: 9,
                    parent_chain_digest: chain_one,
                    chain_digest: chain_two,
                    pack_digest: second.metadata.sha256,
                    pack: second.metadata.clone(),
                },
            ],
            head_sequence: 9,
            head_digest: chain_two,
            terminal_cursor,
        };
        manifest.validate().unwrap();
        let mut files = Vec::new();
        for (name, path) in [
            ("base", base.path),
            ("delta-0001", first.path),
            ("delta-0002", second.path),
        ] {
            files.push((name.to_owned(), fs::read(path).unwrap()));
        }
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        files.push(("manifest.json".into(), [manifest_bytes, b"\n".to_vec()].concat()));
        files.sort_by(|left, right| left.0.cmp(&right.0));
        (manifest, files)
    }

    fn write_fixture_set(root: &Path) {
        if root.exists() {
            fs::remove_dir_all(root).unwrap();
        }
        fs::create_dir_all(root.join("v1/packs/sha256")).unwrap();
        let (manifest, files) = fixture_lineage(&root.join("work"));
        let mut claims = Vec::new();
        for pack in manifest
            .base
            .packs
            .iter()
            .chain(manifest.deltas.iter().map(|delta| &delta.pack))
        {
            let bytes = fs::read(
                root.join("work/build/packs/sha256")
                    .join(format!("{}.sqlite.zst", pack.sha256)),
            )
            .unwrap();
            let destination = root
                .join("v1/packs/sha256")
                .join(format!("{}.sqlite.zst", pack.sha256));
            fs::write(&destination, &bytes).unwrap();
            claims.push(format!(
                "{}  {} {}",
                pack.sha256,
                bytes.len(),
                pack.relative_path.trim_start_matches('/')
            ));
        }
        let manifest_bytes = files
            .iter()
            .find(|(name, _)| name == "manifest.json")
            .map(|(_, bytes)| bytes.clone())
            .unwrap();
        fs::write(root.join("manifest.json"), &manifest_bytes).unwrap();
        let digest = Sha256Digest::of_bytes(&manifest_bytes);
        claims.push(format!("{}  {} manifest.json", digest, manifest_bytes.len()));
        claims.sort();
        fs::write(root.join("SHA256SUMS"), format!("{}\n", claims.join("\n"))).unwrap();
        fs::remove_dir_all(root.join("work")).unwrap();
    }

    #[test]
    fn delta_v2_fixtures_regenerate_deterministically_and_validate_lineage() {
        let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/delta-v2");
        let (manifest, expected_files) = fixture_lineage(&tempfile::tempdir().unwrap().path().join("work"));
        manifest.validate().unwrap();
        for (name, expected) in expected_files {
            let actual = match name.as_str() {
                "manifest.json" => fs::read(fixture_root.join("manifest.json")).unwrap(),
                _ => {
                    let pack = manifest
                        .base
                        .packs
                        .iter()
                        .chain(manifest.deltas.iter().map(|delta| &delta.pack))
                        .find(|pack| match name.as_str() {
                            "base" => **pack == manifest.base.packs[0],
                            "delta-0001" => **pack == manifest.deltas[0].pack,
                            _ => **pack == manifest.deltas[1].pack,
                        })
                        .unwrap();
                    fs::read(
                        fixture_root
                            .join("v1/packs/sha256")
                            .join(format!("{}.sqlite.zst", pack.sha256)),
                    )
                    .unwrap()
                }
            };
            assert_eq!(actual, expected, "fixture {name}");
        }
        let parsed: LineageManifestV2 =
            serde_json::from_slice(&fs::read(fixture_root.join("manifest.json")).unwrap())
                .unwrap();
        parsed.validate().unwrap();
    }

    #[test]
    #[ignore = "fixture writer; run explicitly when refreshing committed golden files"]
    fn write_delta_v2_fixtures() {
        let hub_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/delta-v2");
        write_fixture_set(&hub_root);
        if let Ok(client_root) = env::var("TESLATLAS_CLIENT_FIXTURES") {
            write_fixture_set(Path::new(&client_root));
        }
    }
}
