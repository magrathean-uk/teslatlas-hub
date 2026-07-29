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
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::protocol::{
    CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V1, MirrorTable, OpaqueCursor, PROTOCOL_V1,
    PackCompression, PackFormat, ProtocolError, ProtocolLimits,
    SQLITE_HUB_PROJECTION_APPLICATION_ID, SequenceRange, Sha256Digest, SyncManifest, TransportPack,
    VerifiedTransportPack,
};

const COMPRESSION_LEVEL: i32 = 3;
const MAX_TEXT_BYTES: usize = 16 * 1024;

/// The stable Hub identities a pack is bound to. One pack is for one vehicle
/// and one local mirror car ID, not an account-wide database copy.
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectionCar {
    pub id: i64,
    pub name: String,
    pub model: String,
    pub vin: Option<String>,
    pub firmware_version: Option<String>,
    pub efficiency_wh_per_km: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
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
    pub speed_max: Option<i64>,
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
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectionPosition {
    pub id: i64,
    pub drive_id: i64,
    pub car_id: i64,
    pub date_ms: i64,
    pub latitude: f64,
    pub longitude: f64,
    pub speed: Option<i64>,
    pub power: Option<i64>,
    pub battery_level: Option<i64>,
    pub usable_battery_level: Option<i64>,
    pub elevation: Option<i64>,
    pub odometer: Option<f64>,
    pub ideal_battery_range_km: Option<f64>,
    pub rated_battery_range_km: Option<f64>,
    pub is_climate_on: Option<bool>,
    pub inside_temp: Option<f64>,
    pub outside_temp: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectionCharge {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
    pub end_date_ms: Option<i64>,
    pub charge_energy_added: Option<f64>,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
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

    let mut total_compressed_bytes = 0_u64;
    let mut total_uncompressed_bytes = 0_u64;
    let mut total_rows = 0_u64;
    let mut metadata = Vec::with_capacity(chunks.len());
    for (expected_ordinal, built) in chunks.iter().enumerate() {
        let pack = &built.metadata;
        if pack.snapshot_id != snapshot_id
            || pack.schema != HUB_PROJECTION_SCHEMA_V1
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
        total_rows = total_rows
            .checked_add(pack.row_count)
            .ok_or(ProjectionPackError::ManifestTotalsOverflow)?;
        metadata.push(pack.clone());
    }

    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V1,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: sequence.to_inclusive,
        },
    )?;
    let manifest = SyncManifest {
        protocol: PROTOCOL_V1,
        schema: HUB_PROJECTION_SCHEMA_V1,
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
}

impl ProjectionPackWriter {
    pub fn new(packs_dir: impl Into<PathBuf>) -> Self {
        Self {
            packs_dir: packs_dir.into(),
            limits: ProtocolLimits::default(),
        }
    }

    pub fn with_limits(packs_dir: impl Into<PathBuf>, limits: ProtocolLimits) -> Self {
        Self {
            packs_dir: packs_dir.into(),
            limits,
        }
    }

    pub fn content_path(&self, digest: Sha256Digest) -> PathBuf {
        self.packs_dir
            .join("sha256")
            .join(format!("{digest}.sqlite.zst"))
    }

    /// Write and verify an immutable, complete mirror snapshot. The caller
    /// supplies a bounded projection; the writer never inspects raw telemetry.
    pub fn write_full_snapshot(
        &self,
        request: &ProjectionPackRequest<'_>,
    ) -> Result<BuiltProjectionPack, ProjectionPackError> {
        validate_request(request, self.limits)?;
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
        write_projection_sqlite(sqlite_temp.path(), request, self.limits)?;
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
            tables: tables_for_snapshot(request.snapshot),
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
        if !drive_ids.contains(&position.drive_id) {
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

fn tables_for_snapshot(snapshot: &ProjectionSnapshot) -> Vec<MirrorTable> {
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
    tables
}

fn write_projection_sqlite(
    path: &Path,
    request: &ProjectionPackRequest<'_>,
    limits: ProtocolLimits,
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
                firmware_version TEXT,
                efficiency_wh_per_km REAL
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
                speed_max INTEGER,
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
                end_rated_range_km REAL
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE charges (
                id INTEGER PRIMARY KEY,
                car_id INTEGER NOT NULL REFERENCES cars(id),
                start_date_ms INTEGER NOT NULL,
                end_date_ms INTEGER,
                charge_energy_added REAL,
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
                drive_id INTEGER NOT NULL REFERENCES drives(id),
                car_id INTEGER NOT NULL REFERENCES cars(id),
                date_ms INTEGER NOT NULL,
                latitude REAL NOT NULL,
                longitude REAL NOT NULL,
                speed INTEGER,
                power INTEGER,
                battery_level INTEGER,
                usable_battery_level INTEGER,
                elevation INTEGER,
                odometer REAL,
                ideal_battery_range_km REAL,
                rated_battery_range_km REAL,
                is_climate_on INTEGER CHECK (is_climate_on IN (0, 1)),
                inside_temp REAL,
                outside_temp REAL
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

    let transaction = connection
        .unchecked_transaction()
        .map_err(ProjectionPackError::BeginTransaction)?;
    insert_metadata(&transaction, request)?;
    insert_cars(&transaction, &request.snapshot.cars)?;
    insert_drives(&transaction, &request.snapshot.drives)?;
    insert_charges(&transaction, &request.snapshot.charges)?;
    insert_positions(&transaction, &request.snapshot.positions)?;
    insert_charge_samples(&transaction, &request.snapshot.charge_samples)?;
    transaction.commit().map_err(ProjectionPackError::Commit)?;
    connection
        .execute_batch("PRAGMA optimize; VACUUM;")
        .map_err(ProjectionPackError::FinalizeSqlite)?;
    connection
        .pragma_update(None, "application_id", SQLITE_HUB_PROJECTION_APPLICATION_ID)
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    connection
        .pragma_update(
            None,
            "user_version",
            HUB_PROJECTION_SCHEMA_V1.sqlite_user_version(),
        )
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
) -> Result<(), ProjectionPackError> {
    let values = [
        ("protocol", "teslatlas-sync".to_owned()),
        ("pack_format", "hub_projection_sqlite".to_owned()),
        ("schema_major", HUB_PROJECTION_SCHEMA_V1.major.to_string()),
        ("schema_minor", HUB_PROJECTION_SCHEMA_V1.minor.to_string()),
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
        ("row_count", request.snapshot.row_count()?.to_string()),
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

fn insert_cars(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCar],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO cars (id, name, model, vin, firmware_version, efficiency_wh_per_km)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.name,
                row.model,
                row.vin,
                row.firmware_version,
                row.efficiency_wh_per_km,
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
                duration_min, efficiency, outside_temp_avg, speed_max, start_address,
                end_address, start_geofence, end_geofence, start_latitude, start_longitude,
                end_latitude, end_longitude, start_soc, end_soc, start_rated_range_km,
                end_rated_range_km
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
                row.car_id,
                row.optimized_at_ms,
                row.start_date_ms,
                row.end_date_ms,
                row.distance_km,
                row.duration_min,
                row.efficiency,
                row.outside_temp_avg,
                row.speed_max,
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
                start_battery_level, end_battery_level, duration_min, address, location_name,
                geofence, is_dc, charge_rate_km_per_hour, max_charger_power_kw,
                outside_temp_avg, start_rated_range_km, end_rated_range_km
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17
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
                ideal_battery_range_km, rated_battery_range_km, is_climate_on,
                inside_temp, outside_temp
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17
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
                row.rated_battery_range_km,
                bool_as_sql(row.is_climate_on),
                row.inside_temp,
                row.outside_temp,
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

fn publish_immutable(
    temporary_path: &Path,
    final_path: &Path,
    metadata: &TransportPack,
    limits: ProtocolLimits,
) -> Result<(), ProjectionPackError> {
    match fs::hard_link(temporary_path, final_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            verify_file(metadata, final_path, limits).map(|_| ())
        }
        Err(source) => Err(ProjectionPackError::Publish {
            path: final_path.to_path_buf(),
            source,
        }),
    }
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
            match OpenOptions::new().write(true).create_new(true).open(&path) {
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
    use super::*;

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
                firmware_version: Some("2026.1.1".into()),
                efficiency_wh_per_km: Some(145.0),
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
                speed_max: Some(80),
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
            }],
            positions: vec![ProjectionPosition {
                id: 30,
                drive_id: 20,
                car_id: 10,
                date_ms: 1_700_000_030_000,
                latitude: 51.505,
                longitude: -0.105,
                speed: Some(40),
                power: Some(3),
                battery_level: Some(78),
                usable_battery_level: Some(77),
                elevation: Some(25),
                odometer: Some(10_000.5),
                ideal_battery_range_km: Some(390.0),
                rated_battery_range_km: Some(388.0),
                is_climate_on: Some(false),
                inside_temp: Some(20.0),
                outside_temp: Some(18.0),
            }],
            charges: vec![ProjectionCharge {
                id: 40,
                car_id: 10,
                start_date_ms: 1_700_001_000_000,
                end_date_ms: Some(1_700_001_360_000),
                charge_energy_added: Some(20.0),
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
            &key,
        )
        .unwrap();
        assert_eq!(manifest.chunk_count, 2);
        assert_eq!(manifest.chunks[0].ordinal, 0);
        assert_eq!(manifest.chunks[1].ordinal, 1);
        assert_eq!(
            manifest.total_rows,
            first.metadata.row_count + second.metadata.row_count
        );
        manifest.validate_terminal_cursor(&key).unwrap();
    }

    #[test]
    fn rejects_a_position_without_its_drive_or_valid_coordinates() {
        let mut source = snapshot();
        source.positions[0].drive_id = 999;
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
}
