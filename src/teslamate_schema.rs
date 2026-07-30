//! Read-only TeslaMate schema contract.
//!
//! This module is deliberately only a contract: it never opens a socket, builds
//! a connection string, receives a credential, or executes SQL. The importer
//! must run [`READ_ONLY_SESSION_SQL`] first, issue the fixed probe queries,
//! validate their rows with this module, and only then use the fixed paginated
//! projections below.
//!
//! The contract covers TeslaMate v4.0.1 (`20260411070212`) through upstream
//! main as inspected at `20260718160000`. A newer migration is rejected until
//! its schema delta has been reviewed; additive-looking migrations are not
//! assumed safe by a telemetry importer.

/// First TeslaMate migration version this adapter supports (v4.0.1).
pub const MIN_SUPPORTED_MIGRATION: i64 = 20_260_411_070_212;

/// Last TeslaMate migration version reviewed for this adapter.
pub const MAX_VALIDATED_MIGRATION: i64 = 20_260_718_160_000;

/// The only session statements a PostgreSQL reader may execute before probing.
pub const READ_ONLY_SESSION_SQL: [&str; 4] = [
    "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
    "BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY",
    "SET LOCAL TIME ZONE 'UTC'",
    "SET LOCAL lock_timeout = '5s'; SET LOCAL statement_timeout = '10min'",
];

/// Read the installed Ecto migration high-water mark from the exact source
/// schema. No unqualified relation is ever used.
pub const MIGRATION_VERSION_SQL: &str = r#"
SELECT MAX("migration"."version")::bigint AS "version"
FROM "public"."schema_migrations" AS "migration"
"#;

/// Describe every column in the source telemetry tables through PostgreSQL's
/// system catalogues. This is a metadata-only query; it cannot change source
/// data and it cannot be influenced by an identifier supplied at runtime.
pub const SCHEMA_PROBE_SQL: &str = r#"
SELECT
  "relation"."relname" AS "table_name",
  "attribute"."attname" AS "column_name",
  "type"."typname" AS "type_name",
  NOT "attribute"."attnotnull" AS "is_nullable"
FROM "pg_catalog"."pg_class" AS "relation"
JOIN "pg_catalog"."pg_namespace" AS "namespace"
  ON "namespace"."oid" = "relation"."relnamespace"
JOIN "pg_catalog"."pg_attribute" AS "attribute"
  ON "attribute"."attrelid" = "relation"."oid"
JOIN "pg_catalog"."pg_type" AS "type"
  ON "type"."oid" = "attribute"."atttypid"
WHERE "namespace"."nspname" = 'public'
  AND "relation"."relkind" IN ('r', 'p')
  AND "relation"."relname" = ANY (
    ARRAY[
      'cars',
      'drives',
      'positions',
      'charging_processes',
      'charges',
      'addresses',
      'geofences',
      'states',
      'updates'
    ]::text[]
  )
  AND "attribute"."attnum" > 0
  AND NOT "attribute"."attisdropped"
ORDER BY "relation"."relname", "attribute"."attnum"
"#;

/// A fixed source table. There is deliberately no `Other(String)` variant:
/// callers cannot turn a user supplied identifier into SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceTable {
    Cars,
    Drives,
    Positions,
    ChargingProcesses,
    Charges,
    Addresses,
    Geofences,
    States,
    Updates,
}

impl SourceTable {
    pub const ALL: [Self; 9] = [
        Self::Cars,
        Self::Drives,
        Self::Positions,
        Self::ChargingProcesses,
        Self::Charges,
        Self::Addresses,
        Self::Geofences,
        Self::States,
        Self::Updates,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Cars => "cars",
            Self::Drives => "drives",
            Self::Positions => "positions",
            Self::ChargingProcesses => "charging_processes",
            Self::Charges => "charges",
            Self::Addresses => "addresses",
            Self::Geofences => "geofences",
            Self::States => "states",
            Self::Updates => "updates",
        }
    }
}

/// Logical PostgreSQL source types accepted by the adapter. The importer must
/// bind and decode using the inspected type; these are compatibility guards,
/// not a request to cast the source data in SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    SmallInt,
    Integer,
    BigInt,
    Floating,
    Numeric,
    Boolean,
    Text,
    Timestamp,
    StateStatus,
}

impl ValueType {
    /// Return whether a PostgreSQL `pg_type.typname` belongs to this supported
    /// source type family.
    pub fn accepts_udt(self, type_name: &str) -> bool {
        match self {
            Self::SmallInt => matches!(type_name, "int2"),
            Self::Integer => matches!(type_name, "int4"),
            Self::BigInt => matches!(type_name, "int8"),
            Self::Floating => matches!(type_name, "float8"),
            Self::Numeric => matches!(type_name, "numeric"),
            Self::Boolean => matches!(type_name, "bool"),
            Self::Text => matches!(type_name, "text" | "varchar"),
            Self::Timestamp => matches!(type_name, "timestamp"),
            Self::StateStatus => matches!(type_name, "states_status"),
        }
    }

    /// Canonical PostgreSQL `typname` used by synthetic probe tests.
    pub const fn canonical_udt(self) -> &'static str {
        match self {
            Self::SmallInt => "int2",
            Self::Integer => "int4",
            Self::BigInt => "int8",
            Self::Floating => "float8",
            Self::Numeric => "numeric",
            Self::Boolean => "bool",
            Self::Text => "text",
            Self::Timestamp => "timestamp",
            Self::StateStatus => "states_status",
        }
    }
}

/// One explicit output field from a source table. `nullable` describes source
/// data, not whether the relation column itself must exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionColumn {
    pub source_name: &'static str,
    pub output_name: &'static str,
    pub value_type: ValueType,
    pub nullable: bool,
}

/// A fully fixed, keyset-paginated source projection. `$1` is the exclusive
/// last id, `$2` is the bounded page size, and `$3` is the selected TeslaMate
/// car id. No caller input is interpolated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableProjection {
    pub table: SourceTable,
    pub sql: &'static str,
    pub columns: &'static [ProjectionColumn],
}

macro_rules! column {
    ($name:literal, $type:ident, $nullable:expr) => {
        ProjectionColumn {
            source_name: $name,
            output_name: $name,
            value_type: ValueType::$type,
            nullable: $nullable,
        }
    };
}

const CARS_COLUMNS: &[ProjectionColumn] = &[
    column!("id", SmallInt, false),
    column!("eid", BigInt, false),
    column!("vid", BigInt, false),
    column!("vin", Text, true),
    column!("name", Text, true),
    column!("model", Text, true),
    column!("efficiency", Floating, true),
    column!("trim_badging", Text, true),
    column!("marketing_name", Text, true),
    column!("exterior_color", Text, true),
    column!("wheel_type", Text, true),
    column!("spoiler_type", Text, true),
    column!("display_priority", SmallInt, false),
    column!("inserted_at", Timestamp, false),
    column!("updated_at", Timestamp, false),
];

const DRIVES_COLUMNS: &[ProjectionColumn] = &[
    column!("id", Integer, false),
    column!("car_id", SmallInt, false),
    column!("start_date", Timestamp, false),
    column!("end_date", Timestamp, true),
    column!("start_position_id", Integer, true),
    column!("end_position_id", Integer, true),
    column!("start_address_id", Integer, true),
    column!("end_address_id", Integer, true),
    column!("start_geofence_id", Integer, true),
    column!("end_geofence_id", Integer, true),
    column!("outside_temp_avg", Numeric, true),
    column!("inside_temp_avg", Numeric, true),
    column!("speed_max", SmallInt, true),
    column!("power_max", SmallInt, true),
    column!("power_min", SmallInt, true),
    column!("start_ideal_range_km", Numeric, true),
    column!("end_ideal_range_km", Numeric, true),
    column!("start_rated_range_km", Numeric, true),
    column!("end_rated_range_km", Numeric, true),
    column!("start_km", Floating, true),
    column!("end_km", Floating, true),
    column!("distance", Floating, true),
    column!("duration_min", SmallInt, true),
    column!("ascent", SmallInt, true),
    column!("descent", SmallInt, true),
];

const POSITIONS_COLUMNS: &[ProjectionColumn] = &[
    column!("id", Integer, false),
    column!("car_id", SmallInt, false),
    column!("drive_id", Integer, true),
    column!("date", Timestamp, false),
    column!("latitude", Numeric, false),
    column!("longitude", Numeric, false),
    column!("elevation", SmallInt, true),
    column!("speed", SmallInt, true),
    column!("power", SmallInt, true),
    column!("odometer", Floating, true),
    column!("ideal_battery_range_km", Numeric, true),
    column!("est_battery_range_km", Numeric, true),
    column!("rated_battery_range_km", Numeric, true),
    column!("battery_level", SmallInt, true),
    column!("usable_battery_level", SmallInt, true),
    column!("battery_heater", Boolean, true),
    column!("battery_heater_on", Boolean, true),
    column!("battery_heater_no_power", Boolean, true),
    column!("outside_temp", Numeric, true),
    column!("inside_temp", Numeric, true),
    column!("fan_status", Integer, true),
    column!("driver_temp_setting", Numeric, true),
    column!("passenger_temp_setting", Numeric, true),
    column!("is_climate_on", Boolean, true),
    column!("is_rear_defroster_on", Boolean, true),
    column!("is_front_defroster_on", Boolean, true),
    column!("tpms_pressure_fl", Numeric, true),
    column!("tpms_pressure_fr", Numeric, true),
    column!("tpms_pressure_rl", Numeric, true),
    column!("tpms_pressure_rr", Numeric, true),
];

const CHARGING_PROCESSES_COLUMNS: &[ProjectionColumn] = &[
    column!("id", Integer, false),
    column!("car_id", SmallInt, false),
    column!("position_id", Integer, false),
    column!("address_id", Integer, true),
    column!("geofence_id", Integer, true),
    column!("start_date", Timestamp, false),
    column!("end_date", Timestamp, true),
    column!("charge_energy_added", Numeric, true),
    column!("charge_energy_used", Numeric, true),
    column!("start_ideal_range_km", Numeric, true),
    column!("end_ideal_range_km", Numeric, true),
    column!("start_rated_range_km", Numeric, true),
    column!("end_rated_range_km", Numeric, true),
    column!("start_battery_level", SmallInt, true),
    column!("end_battery_level", SmallInt, true),
    column!("duration_min", SmallInt, true),
    column!("outside_temp_avg", Numeric, true),
    column!("cost", Numeric, true),
];

const CHARGES_COLUMNS: &[ProjectionColumn] = &[
    column!("id", Integer, false),
    column!("charging_process_id", Integer, false),
    column!("date", Timestamp, false),
    column!("battery_heater", Boolean, true),
    column!("battery_heater_on", Boolean, true),
    column!("battery_heater_no_power", Boolean, true),
    column!("battery_level", SmallInt, true),
    column!("usable_battery_level", SmallInt, true),
    column!("charge_energy_added", Numeric, false),
    column!("charger_actual_current", SmallInt, true),
    column!("charger_phases", SmallInt, true),
    column!("charger_pilot_current", SmallInt, true),
    column!("charger_power", SmallInt, false),
    column!("charger_voltage", SmallInt, true),
    column!("conn_charge_cable", Text, true),
    column!("fast_charger_present", Boolean, true),
    column!("fast_charger_brand", Text, true),
    column!("fast_charger_type", Text, true),
    column!("ideal_battery_range_km", Numeric, false),
    column!("rated_battery_range_km", Numeric, true),
    column!("not_enough_power_to_heat", Boolean, true),
    column!("outside_temp", Numeric, true),
];

// Only the local presentation fields used by the typed mirror are read.
// `raw` is deliberately excluded: it can contain unrelated geocoder data.
const ADDRESSES_COLUMNS: &[ProjectionColumn] = &[
    column!("id", Integer, false),
    column!("display_name", Text, true),
    column!("name", Text, true),
];

const GEOFENCES_COLUMNS: &[ProjectionColumn] =
    &[column!("id", Integer, false), column!("name", Text, false)];

const STATES_COLUMNS: &[ProjectionColumn] = &[
    column!("id", Integer, false),
    column!("car_id", SmallInt, false),
    column!("state", StateStatus, false),
    column!("start_date", Timestamp, false),
    column!("end_date", Timestamp, true),
];

const UPDATES_COLUMNS: &[ProjectionColumn] = &[
    column!("id", Integer, false),
    column!("car_id", SmallInt, false),
    column!("start_date", Timestamp, false),
    column!("end_date", Timestamp, true),
    column!("version", Text, true),
];

const CARS_SQL: &str = r#"
SELECT
  "source"."id" AS "id",
  "source"."eid" AS "eid",
  "source"."vid" AS "vid",
  "source"."vin" AS "vin",
  "source"."name" AS "name",
  "source"."model" AS "model",
  "source"."efficiency" AS "efficiency",
  "source"."trim_badging" AS "trim_badging",
  "source"."marketing_name" AS "marketing_name",
  "source"."exterior_color" AS "exterior_color",
  "source"."wheel_type" AS "wheel_type",
  "source"."spoiler_type" AS "spoiler_type",
  "source"."display_priority" AS "display_priority",
  "source"."inserted_at" AS "inserted_at",
  "source"."updated_at" AS "updated_at"
FROM "public"."cars" AS "source"
WHERE "source"."id" > $1
  AND "source"."id" = $3
ORDER BY "source"."id" ASC
LIMIT $2
"#;

const DRIVES_SQL: &str = r#"
SELECT
  "source"."id" AS "id",
  "source"."car_id" AS "car_id",
  "source"."start_date" AS "start_date",
  "source"."end_date" AS "end_date",
  "source"."start_position_id" AS "start_position_id",
  "source"."end_position_id" AS "end_position_id",
  "source"."start_address_id" AS "start_address_id",
  "source"."end_address_id" AS "end_address_id",
  "source"."start_geofence_id" AS "start_geofence_id",
  "source"."end_geofence_id" AS "end_geofence_id",
  "source"."outside_temp_avg" AS "outside_temp_avg",
  "source"."inside_temp_avg" AS "inside_temp_avg",
  "source"."speed_max" AS "speed_max",
  "source"."power_max" AS "power_max",
  "source"."power_min" AS "power_min",
  "source"."start_ideal_range_km" AS "start_ideal_range_km",
  "source"."end_ideal_range_km" AS "end_ideal_range_km",
  "source"."start_rated_range_km" AS "start_rated_range_km",
  "source"."end_rated_range_km" AS "end_rated_range_km",
  "source"."start_km" AS "start_km",
  "source"."end_km" AS "end_km",
  "source"."distance" AS "distance",
  "source"."duration_min" AS "duration_min",
  "source"."ascent" AS "ascent",
  "source"."descent" AS "descent"
FROM "public"."drives" AS "source"
WHERE "source"."id" > $1
  AND "source"."car_id" = $3
ORDER BY "source"."id" ASC
LIMIT $2
"#;

const POSITIONS_SQL: &str = r#"
SELECT
  "source"."id" AS "id",
  "source"."car_id" AS "car_id",
  "source"."drive_id" AS "drive_id",
  "source"."date" AS "date",
  "source"."latitude" AS "latitude",
  "source"."longitude" AS "longitude",
  "source"."elevation" AS "elevation",
  "source"."speed" AS "speed",
  "source"."power" AS "power",
  "source"."odometer" AS "odometer",
  "source"."ideal_battery_range_km" AS "ideal_battery_range_km",
  "source"."est_battery_range_km" AS "est_battery_range_km",
  "source"."rated_battery_range_km" AS "rated_battery_range_km",
  "source"."battery_level" AS "battery_level",
  "source"."usable_battery_level" AS "usable_battery_level",
  "source"."battery_heater" AS "battery_heater",
  "source"."battery_heater_on" AS "battery_heater_on",
  "source"."battery_heater_no_power" AS "battery_heater_no_power",
  "source"."outside_temp" AS "outside_temp",
  "source"."inside_temp" AS "inside_temp",
  "source"."fan_status" AS "fan_status",
  "source"."driver_temp_setting" AS "driver_temp_setting",
  "source"."passenger_temp_setting" AS "passenger_temp_setting",
  "source"."is_climate_on" AS "is_climate_on",
  "source"."is_rear_defroster_on" AS "is_rear_defroster_on",
  "source"."is_front_defroster_on" AS "is_front_defroster_on",
  "source"."tpms_pressure_fl" AS "tpms_pressure_fl",
  "source"."tpms_pressure_fr" AS "tpms_pressure_fr",
  "source"."tpms_pressure_rl" AS "tpms_pressure_rl",
  "source"."tpms_pressure_rr" AS "tpms_pressure_rr"
FROM "public"."positions" AS "source"
WHERE "source"."id" > $1
  AND "source"."car_id" = $3
ORDER BY "source"."id" ASC
LIMIT $2
"#;

const CHARGING_PROCESSES_SQL: &str = r#"
SELECT
  "source"."id" AS "id",
  "source"."car_id" AS "car_id",
  "source"."position_id" AS "position_id",
  "source"."address_id" AS "address_id",
  "source"."geofence_id" AS "geofence_id",
  "source"."start_date" AS "start_date",
  "source"."end_date" AS "end_date",
  "source"."charge_energy_added" AS "charge_energy_added",
  "source"."charge_energy_used" AS "charge_energy_used",
  "source"."start_ideal_range_km" AS "start_ideal_range_km",
  "source"."end_ideal_range_km" AS "end_ideal_range_km",
  "source"."start_rated_range_km" AS "start_rated_range_km",
  "source"."end_rated_range_km" AS "end_rated_range_km",
  "source"."start_battery_level" AS "start_battery_level",
  "source"."end_battery_level" AS "end_battery_level",
  "source"."duration_min" AS "duration_min",
  "source"."outside_temp_avg" AS "outside_temp_avg",
  "source"."cost" AS "cost"
FROM "public"."charging_processes" AS "source"
WHERE "source"."id" > $1
  AND "source"."car_id" = $3
ORDER BY "source"."id" ASC
LIMIT $2
"#;

const CHARGES_SQL: &str = r#"
SELECT
  "source"."id" AS "id",
  "source"."charging_process_id" AS "charging_process_id",
  "source"."date" AS "date",
  "source"."battery_heater" AS "battery_heater",
  "source"."battery_heater_on" AS "battery_heater_on",
  "source"."battery_heater_no_power" AS "battery_heater_no_power",
  "source"."battery_level" AS "battery_level",
  "source"."usable_battery_level" AS "usable_battery_level",
  "source"."charge_energy_added" AS "charge_energy_added",
  "source"."charger_actual_current" AS "charger_actual_current",
  "source"."charger_phases" AS "charger_phases",
  "source"."charger_pilot_current" AS "charger_pilot_current",
  "source"."charger_power" AS "charger_power",
  "source"."charger_voltage" AS "charger_voltage",
  "source"."conn_charge_cable" AS "conn_charge_cable",
  "source"."fast_charger_present" AS "fast_charger_present",
  "source"."fast_charger_brand" AS "fast_charger_brand",
  "source"."fast_charger_type" AS "fast_charger_type",
  "source"."ideal_battery_range_km" AS "ideal_battery_range_km",
  "source"."rated_battery_range_km" AS "rated_battery_range_km",
  "source"."not_enough_power_to_heat" AS "not_enough_power_to_heat",
  "source"."outside_temp" AS "outside_temp"
FROM "public"."charges" AS "source"
JOIN "public"."charging_processes" AS "process"
  ON "process"."id" = "source"."charging_process_id"
WHERE "source"."id" > $1
  AND "process"."car_id" = $3
ORDER BY "source"."id" ASC
LIMIT $2
"#;

const ADDRESSES_SQL: &str = r#"
SELECT
  "source"."id" AS "id",
  "source"."display_name" AS "display_name",
  "source"."name" AS "name"
FROM "public"."addresses" AS "source"
WHERE "source"."id" > $1
  AND (
    EXISTS (
      SELECT 1
      FROM "public"."drives" AS "drive"
      WHERE "drive"."car_id" = $3
        AND ("drive"."start_address_id" = "source"."id"
          OR "drive"."end_address_id" = "source"."id")
    )
    OR EXISTS (
      SELECT 1
      FROM "public"."charging_processes" AS "charging_process"
      WHERE "charging_process"."car_id" = $3
        AND "charging_process"."address_id" = "source"."id"
    )
  )
ORDER BY "source"."id" ASC
LIMIT $2
"#;

const GEOFENCES_SQL: &str = r#"
SELECT
  "source"."id" AS "id",
  "source"."name" AS "name"
FROM "public"."geofences" AS "source"
WHERE "source"."id" > $1
  AND (
    EXISTS (
      SELECT 1
      FROM "public"."drives" AS "drive"
      WHERE "drive"."car_id" = $3
        AND ("drive"."start_geofence_id" = "source"."id"
          OR "drive"."end_geofence_id" = "source"."id")
    )
    OR EXISTS (
      SELECT 1
      FROM "public"."charging_processes" AS "charging_process"
      WHERE "charging_process"."car_id" = $3
        AND "charging_process"."geofence_id" = "source"."id"
    )
  )
ORDER BY "source"."id" ASC
LIMIT $2
"#;

const STATES_SQL: &str = r#"
SELECT
  "source"."id" AS "id",
  "source"."car_id" AS "car_id",
  "source"."state" AS "state",
  "source"."start_date" AS "start_date",
  "source"."end_date" AS "end_date"
FROM "public"."states" AS "source"
WHERE "source"."id" > $1
  AND "source"."car_id" = $3
ORDER BY "source"."id" ASC
LIMIT $2
"#;

const UPDATES_SQL: &str = r#"
SELECT
  "source"."id" AS "id",
  "source"."car_id" AS "car_id",
  "source"."start_date" AS "start_date",
  "source"."end_date" AS "end_date",
  "source"."version" AS "version"
FROM "public"."updates" AS "source"
WHERE "source"."id" > $1
  AND "source"."car_id" = $3
ORDER BY "source"."id" ASC
LIMIT $2
"#;

const CARS_PROJECTION: TableProjection = TableProjection {
    table: SourceTable::Cars,
    sql: CARS_SQL,
    columns: CARS_COLUMNS,
};
const DRIVES_PROJECTION: TableProjection = TableProjection {
    table: SourceTable::Drives,
    sql: DRIVES_SQL,
    columns: DRIVES_COLUMNS,
};
const POSITIONS_PROJECTION: TableProjection = TableProjection {
    table: SourceTable::Positions,
    sql: POSITIONS_SQL,
    columns: POSITIONS_COLUMNS,
};
const CHARGING_PROCESSES_PROJECTION: TableProjection = TableProjection {
    table: SourceTable::ChargingProcesses,
    sql: CHARGING_PROCESSES_SQL,
    columns: CHARGING_PROCESSES_COLUMNS,
};
const CHARGES_PROJECTION: TableProjection = TableProjection {
    table: SourceTable::Charges,
    sql: CHARGES_SQL,
    columns: CHARGES_COLUMNS,
};
const ADDRESSES_PROJECTION: TableProjection = TableProjection {
    table: SourceTable::Addresses,
    sql: ADDRESSES_SQL,
    columns: ADDRESSES_COLUMNS,
};
const GEOFENCES_PROJECTION: TableProjection = TableProjection {
    table: SourceTable::Geofences,
    sql: GEOFENCES_SQL,
    columns: GEOFENCES_COLUMNS,
};
const STATES_PROJECTION: TableProjection = TableProjection {
    table: SourceTable::States,
    sql: STATES_SQL,
    columns: STATES_COLUMNS,
};
const UPDATES_PROJECTION: TableProjection = TableProjection {
    table: SourceTable::Updates,
    sql: UPDATES_SQL,
    columns: UPDATES_COLUMNS,
};

/// Return the fixed SQL and source descriptor for one supported table.
pub const fn projection(table: SourceTable) -> &'static TableProjection {
    match table {
        SourceTable::Cars => &CARS_PROJECTION,
        SourceTable::Drives => &DRIVES_PROJECTION,
        SourceTable::Positions => &POSITIONS_PROJECTION,
        SourceTable::ChargingProcesses => &CHARGING_PROCESSES_PROJECTION,
        SourceTable::Charges => &CHARGES_PROJECTION,
        SourceTable::Addresses => &ADDRESSES_PROJECTION,
        SourceTable::Geofences => &GEOFENCES_PROJECTION,
        SourceTable::States => &STATES_PROJECTION,
        SourceTable::Updates => &UPDATES_PROJECTION,
    }
}

/// A row returned by [`SCHEMA_PROBE_SQL`]. The caller owns collection and
/// decoding; this module only validates the immutable contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedColumn<'a> {
    pub table: &'a str,
    pub name: &'a str,
    pub type_name: &'a str,
    pub nullable: bool,
}

/// A conservative reason a source schema cannot be read by this adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SchemaCompatibilityError {
    #[error("TeslaMate migration {found} is older than supported migration {minimum}")]
    LegacyMigration { found: i64, minimum: i64 },
    #[error("TeslaMate migration {found} is newer than reviewed migration {maximum}")]
    UnreviewedMigration { found: i64, maximum: i64 },
    #[error("TeslaMate source table {table} is missing")]
    MissingTable { table: &'static str },
    #[error("TeslaMate source column {table}.{column} is missing")]
    MissingColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error(
        "TeslaMate source column {table}.{column} has an incompatible type; expected {expected:?}"
    )]
    IncompatibleColumnType {
        table: &'static str,
        column: &'static str,
        expected: ValueType,
    },
}

/// Reject source databases outside the migration interval reviewed by this
/// adapter. An importer must obtain `found` only from [`MIGRATION_VERSION_SQL`]
/// inside its read-only repeatable-read transaction.
pub const fn validate_migration_version(found: i64) -> Result<(), SchemaCompatibilityError> {
    if found < MIN_SUPPORTED_MIGRATION {
        return Err(SchemaCompatibilityError::LegacyMigration {
            found,
            minimum: MIN_SUPPORTED_MIGRATION,
        });
    }
    if found > MAX_VALIDATED_MIGRATION {
        return Err(SchemaCompatibilityError::UnreviewedMigration {
            found,
            maximum: MAX_VALIDATED_MIGRATION,
        });
    }
    Ok(())
}

/// Validate the source metadata returned by [`SCHEMA_PROBE_SQL`]. Extra
/// tables and columns are allowed; missing or type-incompatible projected
/// fields are not. Nullability is intentionally not enforced: a reader can
/// safely represent a NULL from legacy TeslaMate data, while an absent column
/// cannot be recovered.
pub fn validate_observed_schema(
    observed: &[ObservedColumn<'_>],
) -> Result<(), SchemaCompatibilityError> {
    for table in SourceTable::ALL {
        let expected = projection(table);
        if !observed.iter().any(|column| column.table == table.name()) {
            return Err(SchemaCompatibilityError::MissingTable {
                table: table.name(),
            });
        }

        for expected_column in expected.columns {
            let Some(actual) = observed.iter().find(|column| {
                column.table == table.name() && column.name == expected_column.source_name
            }) else {
                return Err(SchemaCompatibilityError::MissingColumn {
                    table: table.name(),
                    column: expected_column.source_name,
                });
            };

            if !expected_column.value_type.accepts_udt(actual.type_name) {
                return Err(SchemaCompatibilityError::IncompatibleColumnType {
                    table: table.name(),
                    column: expected_column.source_name,
                    expected: expected_column.value_type,
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_schema() -> Vec<ObservedColumn<'static>> {
        let mut result = Vec::new();
        for table in SourceTable::ALL {
            for column in projection(table).columns {
                result.push(ObservedColumn {
                    table: table.name(),
                    name: column.source_name,
                    type_name: column.value_type.canonical_udt(),
                    nullable: column.nullable,
                });
            }
        }
        result
    }

    #[test]
    fn all_queries_are_fixed_qualified_and_keyset_paginated() {
        assert!(MIGRATION_VERSION_SQL.contains("\"public\".\"schema_migrations\""));
        assert!(SCHEMA_PROBE_SQL.contains("\"pg_catalog\".\"pg_class\""));
        assert!(SCHEMA_PROBE_SQL.contains("\"pg_catalog\".\"pg_attribute\""));

        for table in SourceTable::ALL {
            let descriptor = projection(table);
            assert!(descriptor.sql.contains(&format!(
                "FROM \"public\".\"{}\" AS \"source\"",
                table.name()
            )));
            assert!(descriptor.sql.contains("WHERE \"source\".\"id\" > $1"));
            assert!(descriptor.sql.contains("$3"));
            assert!(descriptor.sql.contains("ORDER BY \"source\".\"id\" ASC"));
            assert!(descriptor.sql.contains("LIMIT $2"));
            assert!(!descriptor.sql.contains("SELECT *"));
            assert!(!descriptor.sql.contains("postgres://"));

            for column in descriptor.columns {
                assert!(descriptor.sql.contains(&format!(
                    "\"source\".\"{}\" AS \"{}\"",
                    column.source_name, column.output_name
                )));
            }
        }
    }

    #[test]
    fn projections_cover_the_current_telemetry_tables() {
        let names: Vec<_> = SourceTable::ALL.iter().map(|table| table.name()).collect();
        assert_eq!(
            names,
            [
                "cars",
                "drives",
                "positions",
                "charging_processes",
                "charges",
                "addresses",
                "geofences",
                "states",
                "updates",
            ]
        );
        assert_eq!(projection(SourceTable::Cars).columns.len(), 15);
        assert_eq!(projection(SourceTable::Drives).columns.len(), 25);
        assert_eq!(projection(SourceTable::Positions).columns.len(), 30);
        assert_eq!(projection(SourceTable::ChargingProcesses).columns.len(), 18);
        assert_eq!(projection(SourceTable::Charges).columns.len(), 22);
        assert_eq!(projection(SourceTable::Addresses).columns.len(), 3);
        assert_eq!(projection(SourceTable::Geofences).columns.len(), 2);
        assert_eq!(projection(SourceTable::States).columns.len(), 5);
        assert_eq!(projection(SourceTable::Updates).columns.len(), 5);
    }

    #[test]
    fn reviewed_migration_interval_is_closed_and_conservative() {
        assert_eq!(validate_migration_version(MIN_SUPPORTED_MIGRATION), Ok(()));
        assert_eq!(validate_migration_version(MAX_VALIDATED_MIGRATION), Ok(()));
        assert_eq!(
            validate_migration_version(MIN_SUPPORTED_MIGRATION - 1),
            Err(SchemaCompatibilityError::LegacyMigration {
                found: MIN_SUPPORTED_MIGRATION - 1,
                minimum: MIN_SUPPORTED_MIGRATION,
            })
        );
        assert_eq!(
            validate_migration_version(MAX_VALIDATED_MIGRATION + 1),
            Err(SchemaCompatibilityError::UnreviewedMigration {
                found: MAX_VALIDATED_MIGRATION + 1,
                maximum: MAX_VALIDATED_MIGRATION,
            })
        );
    }

    #[test]
    fn valid_current_schema_is_accepted() {
        assert_eq!(validate_observed_schema(&complete_schema()), Ok(()));
    }

    #[test]
    fn missing_table_column_and_type_are_rejected() {
        let mut missing_table = complete_schema();
        missing_table.retain(|column| column.table != "positions");
        assert_eq!(
            validate_observed_schema(&missing_table),
            Err(SchemaCompatibilityError::MissingTable { table: "positions" })
        );

        let mut missing_column = complete_schema();
        missing_column
            .retain(|column| !(column.table == "charges" && column.name == "charger_power"));
        assert_eq!(
            validate_observed_schema(&missing_column),
            Err(SchemaCompatibilityError::MissingColumn {
                table: "charges",
                column: "charger_power",
            })
        );

        let mut wrong_type = complete_schema();
        let latitude = wrong_type
            .iter_mut()
            .find(|column| column.table == "positions" && column.name == "latitude")
            .expect("latitude exists");
        latitude.type_name = "float8";
        assert_eq!(
            validate_observed_schema(&wrong_type),
            Err(SchemaCompatibilityError::IncompatibleColumnType {
                table: "positions",
                column: "latitude",
                expected: ValueType::Numeric,
            })
        );
    }

    #[test]
    fn text_family_accepts_current_text_and_varchar_columns() {
        assert!(ValueType::Text.accepts_udt("text"));
        assert!(ValueType::Text.accepts_udt("varchar"));
        assert!(!ValueType::Text.accepts_udt("int4"));
    }
}
