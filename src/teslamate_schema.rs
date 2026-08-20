//! Read-only TeslaMate schema contract.
//!
//! This module is deliberately only a contract: it never opens a socket, builds
//! a connection string, receives a credential, or executes SQL. The importer
//! must run [`READ_ONLY_SESSION_SQL`] first, issue the fixed probe queries,
//! validate their rows with this module, and only then use the fixed paginated
//! projections below.
//!
//! The contract is pinned to the TeslaMate v4.1.1 tag revision
//! `d6c43bc8c48784da8f0b701945b80b20911b3d1a`. A source must match that
//! migration high-water mark and complete ordered set.
//! A newer migration, or a database reconstructed with a different migration
//! history, is rejected until its schema delta has been reviewed.

use sha2::{Digest, Sha256};

/// First TeslaMate migration version this adapter supports (the exact v4.1.1 set).
pub const MIN_SUPPORTED_MIGRATION: i64 = 20_260_808_090_000;

/// Last migration in the pinned v4.1.1 source tree.
pub const MAX_VALIDATED_MIGRATION: i64 = MIN_SUPPORTED_MIGRATION;

/// Immutable v4.1.1 source revision behind this compatibility contract.
pub const TESLAMATE_V4_SOURCE_REVISION: &str = "d6c43bc8c48784da8f0b701945b80b20911b3d1a";

/// Number of migrations in the pinned source revision.
pub const TESLAMATE_V4_MIGRATION_COUNT: usize = 105;

/// SHA-256 of the sorted, newline-delimited 14-digit migration versions in
/// the pinned source revision. The exact list—not only its high-water mark—is
/// part of the read-only source admission contract.
pub const TESLAMATE_V4_MIGRATION_SET_SHA256: &str =
    "ea850d1b038c4af950db32e7a0939aa5ebe8f1dcefe5e56dcd592f3451038868";

/// Fixed read-only session statements. The COPY statement timeout is supplied
/// separately from validated import limits so large historical reads can be
/// configured without weakening the short lock-wait bound.
pub const READ_ONLY_SESSION_SQL: [&str; 3] = [
    "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
    "BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY",
    "SET LOCAL TIME ZONE 'UTC'; SET LOCAL lock_timeout = '5s'",
];

/// Read the installed Ecto migration high-water mark from the exact source
/// schema. No unqualified relation is ever used.
pub const MIGRATION_VERSION_SQL: &str = r#"
SELECT MAX("migration"."version")::bigint AS "version"
FROM "public"."schema_migrations" AS "migration"
"#;

/// Read every installed migration version in canonical order. The importer
/// hashes these values locally; no optional PostgreSQL extension is required.
pub const MIGRATION_VERSIONS_SQL: &str = r#"
SELECT "migration"."version"::bigint AS "version"
FROM "public"."schema_migrations" AS "migration"
ORDER BY "migration"."version" ASC
"#;

/// Describe every column in the source telemetry tables through PostgreSQL's
/// system catalogues. This is a metadata-only query; it cannot change source
/// data and it cannot be influenced by an identifier supplied at runtime.
pub const SCHEMA_PROBE_SQL: &str = r#"
SELECT
  "relation"."relname" AS "table_name",
  "attribute"."attname" AS "column_name",
  "type"."typname" AS "type_name",
  "pg_catalog"."format_type"("attribute"."atttypid", "attribute"."atttypmod") AS "format_type",
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
      'car_settings',
      'settings',
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

/// Read the exact enum labels required by the pinned physical schema. Label
/// order is semantically significant to PostgreSQL enum comparison, so the
/// probe retains it rather than merely checking a type name.
pub const ENUM_PROBE_SQL: &str = r#"
SELECT
  "type"."typname" AS "type_name",
  "enum"."enumlabel" AS "label"
FROM "pg_catalog"."pg_type" AS "type"
JOIN "pg_catalog"."pg_enum" AS "enum"
  ON "enum"."enumtypid" = "type"."oid"
JOIN "pg_catalog"."pg_namespace" AS "namespace"
  ON "namespace"."oid" = "type"."typnamespace"
WHERE "type"."typname" = ANY (
  ARRAY[
    'states_status',
    'billing_type',
    'unit_of_length',
    'unit_of_temperature',
    'unit_of_pressure',
    'range'
  ]::text[]
)
  AND "namespace"."nspname" = 'public'
ORDER BY "type"."typname" ASC, "enum"."enumsortorder" ASC
"#;

/// The current source is expected to have exactly one global settings row and
/// every current car must reference a car-settings row. It is a fixed metadata
/// query: no source rows or arbitrary identifiers are accepted from callers.
pub const SETTINGS_RELATIONSHIP_SQL: &str = r#"
SELECT
  (SELECT COUNT(*)::bigint FROM "public"."settings") AS "settings_count",
  (
    SELECT COUNT(*)::bigint
    FROM "public"."cars" AS "car"
    LEFT JOIN "public"."car_settings" AS "setting"
      ON "setting"."id" = "car"."settings_id"
    WHERE "setting"."id" IS NULL
  ) AS "cars_without_settings"
"#;

/// A fixed source table. There is deliberately no `Other(String)` variant:
/// callers cannot turn a user supplied identifier into SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceTable {
    Cars,
    CarSettings,
    Settings,
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
            Self::CarSettings => "car_settings",
            Self::Settings => "settings",
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
    Varchar,
    Jsonb,
    Timestamp,
    StateStatus,
    BillingType,
    UnitOfLength,
    UnitOfTemperature,
    UnitOfPressure,
    PreferredRange,
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
            Self::Varchar => matches!(type_name, "varchar"),
            Self::Jsonb => matches!(type_name, "jsonb"),
            Self::Timestamp => matches!(type_name, "timestamp"),
            Self::StateStatus => matches!(type_name, "states_status"),
            Self::BillingType => matches!(type_name, "billing_type"),
            Self::UnitOfLength => matches!(type_name, "unit_of_length"),
            Self::UnitOfTemperature => matches!(type_name, "unit_of_temperature"),
            Self::UnitOfPressure => matches!(type_name, "unit_of_pressure"),
            Self::PreferredRange => matches!(type_name, "range"),
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
            Self::Varchar => "varchar",
            Self::Jsonb => "jsonb",
            Self::Timestamp => "timestamp",
            Self::StateStatus => "states_status",
            Self::BillingType => "billing_type",
            Self::UnitOfLength => "unit_of_length",
            Self::UnitOfTemperature => "unit_of_temperature",
            Self::UnitOfPressure => "unit_of_pressure",
            Self::PreferredRange => "range",
        }
    }
}

/// One explicit output field from a source table. `nullable` describes source
/// data, not whether the relation column itself must exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionColumn {
    /// `None` means the column belongs to the projection's primary table.
    /// Joined fields name their actual source relation explicitly.
    pub source_table: Option<SourceTable>,
    pub source_name: &'static str,
    pub output_name: &'static str,
    pub value_type: ValueType,
    pub nullable: bool,
}

/// A physical source column whose exact type and nullability are part of the
/// pinned TeslaMate v4 contract even when the Hub does not yet project its
/// value into a phone pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PinnedSourceColumn {
    name: &'static str,
    value_type: ValueType,
    nullable: bool,
    format_type: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PinnedSourceTable {
    table: SourceTable,
    columns: &'static [PinnedSourceColumn],
}

macro_rules! pinned_source_column {
    ($name:literal, $value_type:ident, $nullable:expr, $format_type:literal) => {
        PinnedSourceColumn {
            name: $name,
            value_type: ValueType::$value_type,
            nullable: $nullable,
            format_type: $format_type,
        }
    };
}

const CARS_SOURCE_COLUMNS: &[PinnedSourceColumn] = &[
    pinned_source_column!("id", SmallInt, false, "smallint"),
    pinned_source_column!("eid", BigInt, false, "bigint"),
    pinned_source_column!("vid", BigInt, false, "bigint"),
    pinned_source_column!("vin", Text, false, "text"),
    pinned_source_column!("name", Text, true, "text"),
    pinned_source_column!("model", Varchar, true, "character varying(255)"),
    pinned_source_column!("efficiency", Floating, true, "double precision"),
    pinned_source_column!("trim_badging", Text, true, "text"),
    pinned_source_column!("marketing_name", Varchar, true, "character varying(255)"),
    pinned_source_column!("exterior_color", Text, true, "text"),
    pinned_source_column!("wheel_type", Text, true, "text"),
    pinned_source_column!("spoiler_type", Text, true, "text"),
    pinned_source_column!("display_priority", SmallInt, false, "smallint"),
    pinned_source_column!(
        "inserted_at",
        Timestamp,
        false,
        "timestamp(0) without time zone"
    ),
    pinned_source_column!(
        "updated_at",
        Timestamp,
        false,
        "timestamp(0) without time zone"
    ),
    pinned_source_column!("settings_id", BigInt, false, "bigint"),
];

const CAR_SETTINGS_SOURCE_COLUMNS: &[PinnedSourceColumn] = &[
    PinnedSourceColumn {
        name: "id",
        value_type: ValueType::BigInt,
        nullable: false,
        format_type: "bigint",
    },
    PinnedSourceColumn {
        name: "suspend_min",
        value_type: ValueType::Integer,
        nullable: false,
        format_type: "integer",
    },
    PinnedSourceColumn {
        name: "suspend_after_idle_min",
        value_type: ValueType::Integer,
        nullable: false,
        format_type: "integer",
    },
    PinnedSourceColumn {
        name: "req_not_unlocked",
        value_type: ValueType::Boolean,
        nullable: false,
        format_type: "boolean",
    },
    PinnedSourceColumn {
        name: "free_supercharging",
        value_type: ValueType::Boolean,
        nullable: false,
        format_type: "boolean",
    },
    PinnedSourceColumn {
        name: "use_streaming_api",
        value_type: ValueType::Boolean,
        nullable: false,
        format_type: "boolean",
    },
    PinnedSourceColumn {
        name: "enabled",
        value_type: ValueType::Boolean,
        nullable: false,
        format_type: "boolean",
    },
    PinnedSourceColumn {
        name: "lfp_battery",
        value_type: ValueType::Boolean,
        nullable: false,
        format_type: "boolean",
    },
];

const SETTINGS_SOURCE_COLUMNS: &[PinnedSourceColumn] = &[
    PinnedSourceColumn {
        name: "id",
        value_type: ValueType::BigInt,
        nullable: false,
        format_type: "bigint",
    },
    PinnedSourceColumn {
        name: "unit_of_length",
        value_type: ValueType::UnitOfLength,
        nullable: false,
        format_type: "unit_of_length",
    },
    PinnedSourceColumn {
        name: "unit_of_temperature",
        value_type: ValueType::UnitOfTemperature,
        nullable: false,
        format_type: "unit_of_temperature",
    },
    PinnedSourceColumn {
        name: "unit_of_pressure",
        value_type: ValueType::UnitOfPressure,
        nullable: false,
        format_type: "unit_of_pressure",
    },
    PinnedSourceColumn {
        name: "preferred_range",
        value_type: ValueType::PreferredRange,
        nullable: false,
        format_type: "range",
    },
    PinnedSourceColumn {
        name: "base_url",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "grafana_url",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "language",
        value_type: ValueType::Text,
        nullable: false,
        format_type: "text",
    },
    PinnedSourceColumn {
        name: "theme_mode",
        value_type: ValueType::Text,
        nullable: false,
        format_type: "text",
    },
    PinnedSourceColumn {
        name: "inserted_at",
        value_type: ValueType::Timestamp,
        nullable: false,
        format_type: "timestamp(0) without time zone",
    },
    PinnedSourceColumn {
        name: "updated_at",
        value_type: ValueType::Timestamp,
        nullable: false,
        format_type: "timestamp(0) without time zone",
    },
];

// The selected-car telemetry relation shapes are admission gates, independent
// of the narrower reader projections below. A familiar column name is not
// enough: source precision and nullability must stay exactly pinned.
//
// TeslaMate declares these ten telemetry timestamps without a precision
// typmod. PostgreSQL therefore reports `timestamp without time zone`, not
// `timestamp(6) without time zone`, through `format_type`.
const DRIVES_SOURCE_COLUMNS: &[PinnedSourceColumn] = &[
    pinned_source_column!("id", Integer, false, "integer"),
    pinned_source_column!("car_id", SmallInt, false, "smallint"),
    pinned_source_column!(
        "start_date",
        Timestamp,
        false,
        "timestamp without time zone"
    ),
    pinned_source_column!("end_date", Timestamp, true, "timestamp without time zone"),
    pinned_source_column!("start_position_id", Integer, true, "integer"),
    pinned_source_column!("end_position_id", Integer, true, "integer"),
    pinned_source_column!("start_address_id", Integer, true, "integer"),
    pinned_source_column!("end_address_id", Integer, true, "integer"),
    pinned_source_column!("start_geofence_id", Integer, true, "integer"),
    pinned_source_column!("end_geofence_id", Integer, true, "integer"),
    pinned_source_column!("outside_temp_avg", Numeric, true, "numeric(4,1)"),
    pinned_source_column!("inside_temp_avg", Numeric, true, "numeric(4,1)"),
    pinned_source_column!("speed_max", SmallInt, true, "smallint"),
    pinned_source_column!("power_max", SmallInt, true, "smallint"),
    pinned_source_column!("power_min", SmallInt, true, "smallint"),
    pinned_source_column!("start_ideal_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("end_ideal_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("start_rated_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("end_rated_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("start_km", Floating, true, "double precision"),
    pinned_source_column!("end_km", Floating, true, "double precision"),
    pinned_source_column!("distance", Floating, true, "double precision"),
    pinned_source_column!("duration_min", SmallInt, true, "smallint"),
    pinned_source_column!("ascent", SmallInt, true, "smallint"),
    pinned_source_column!("descent", SmallInt, true, "smallint"),
];

const POSITIONS_SOURCE_COLUMNS: &[PinnedSourceColumn] = &[
    pinned_source_column!("id", Integer, false, "integer"),
    pinned_source_column!("car_id", SmallInt, false, "smallint"),
    pinned_source_column!("drive_id", Integer, true, "integer"),
    pinned_source_column!("date", Timestamp, false, "timestamp without time zone"),
    pinned_source_column!("latitude", Numeric, false, "numeric(8,6)"),
    pinned_source_column!("longitude", Numeric, false, "numeric(9,6)"),
    pinned_source_column!("elevation", SmallInt, true, "smallint"),
    pinned_source_column!("speed", SmallInt, true, "smallint"),
    pinned_source_column!("power", SmallInt, true, "smallint"),
    pinned_source_column!("odometer", Floating, true, "double precision"),
    pinned_source_column!("ideal_battery_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("est_battery_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("rated_battery_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("battery_level", SmallInt, true, "smallint"),
    pinned_source_column!("usable_battery_level", SmallInt, true, "smallint"),
    pinned_source_column!("battery_heater", Boolean, true, "boolean"),
    pinned_source_column!("battery_heater_on", Boolean, true, "boolean"),
    pinned_source_column!("battery_heater_no_power", Boolean, true, "boolean"),
    pinned_source_column!("outside_temp", Numeric, true, "numeric(4,1)"),
    pinned_source_column!("inside_temp", Numeric, true, "numeric(4,1)"),
    pinned_source_column!("fan_status", Integer, true, "integer"),
    pinned_source_column!("driver_temp_setting", Numeric, true, "numeric(4,1)"),
    pinned_source_column!("passenger_temp_setting", Numeric, true, "numeric(4,1)"),
    pinned_source_column!("is_climate_on", Boolean, true, "boolean"),
    pinned_source_column!("is_rear_defroster_on", Boolean, true, "boolean"),
    pinned_source_column!("is_front_defroster_on", Boolean, true, "boolean"),
    pinned_source_column!("tpms_pressure_fl", Numeric, true, "numeric(4,1)"),
    pinned_source_column!("tpms_pressure_fr", Numeric, true, "numeric(4,1)"),
    pinned_source_column!("tpms_pressure_rl", Numeric, true, "numeric(4,1)"),
    pinned_source_column!("tpms_pressure_rr", Numeric, true, "numeric(4,1)"),
];

const CHARGING_PROCESSES_SOURCE_COLUMNS: &[PinnedSourceColumn] = &[
    pinned_source_column!("id", Integer, false, "integer"),
    pinned_source_column!("car_id", SmallInt, false, "smallint"),
    pinned_source_column!("position_id", Integer, false, "integer"),
    pinned_source_column!("address_id", Integer, true, "integer"),
    pinned_source_column!("geofence_id", Integer, true, "integer"),
    pinned_source_column!(
        "start_date",
        Timestamp,
        false,
        "timestamp without time zone"
    ),
    pinned_source_column!("end_date", Timestamp, true, "timestamp without time zone"),
    pinned_source_column!("charge_energy_added", Numeric, true, "numeric(8,2)"),
    pinned_source_column!("charge_energy_used", Numeric, true, "numeric(8,2)"),
    pinned_source_column!("start_ideal_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("end_ideal_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("start_rated_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("end_rated_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("start_battery_level", SmallInt, true, "smallint"),
    pinned_source_column!("end_battery_level", SmallInt, true, "smallint"),
    pinned_source_column!("duration_min", SmallInt, true, "smallint"),
    pinned_source_column!("outside_temp_avg", Numeric, true, "numeric(4,1)"),
    pinned_source_column!("cost", Numeric, true, "numeric(14,2)"),
];

const CHARGES_SOURCE_COLUMNS: &[PinnedSourceColumn] = &[
    pinned_source_column!("id", Integer, false, "integer"),
    pinned_source_column!("charging_process_id", Integer, false, "integer"),
    pinned_source_column!("date", Timestamp, false, "timestamp without time zone"),
    pinned_source_column!("battery_heater", Boolean, true, "boolean"),
    pinned_source_column!("battery_heater_on", Boolean, true, "boolean"),
    pinned_source_column!("battery_heater_no_power", Boolean, true, "boolean"),
    pinned_source_column!("battery_level", SmallInt, true, "smallint"),
    pinned_source_column!("usable_battery_level", SmallInt, true, "smallint"),
    pinned_source_column!("charge_energy_added", Numeric, false, "numeric(8,2)"),
    pinned_source_column!("charger_actual_current", SmallInt, true, "smallint"),
    pinned_source_column!("charger_phases", SmallInt, true, "smallint"),
    pinned_source_column!("charger_pilot_current", SmallInt, true, "smallint"),
    pinned_source_column!("charger_power", SmallInt, false, "smallint"),
    pinned_source_column!("charger_voltage", SmallInt, true, "smallint"),
    pinned_source_column!("conn_charge_cable", Varchar, true, "character varying(255)"),
    pinned_source_column!("fast_charger_present", Boolean, true, "boolean"),
    pinned_source_column!(
        "fast_charger_brand",
        Varchar,
        true,
        "character varying(255)"
    ),
    pinned_source_column!("fast_charger_type", Varchar, true, "character varying(255)"),
    pinned_source_column!("ideal_battery_range_km", Numeric, false, "numeric(6,2)"),
    pinned_source_column!("rated_battery_range_km", Numeric, true, "numeric(6,2)"),
    pinned_source_column!("not_enough_power_to_heat", Boolean, true, "boolean"),
    pinned_source_column!("outside_temp", Numeric, true, "numeric(4,1)"),
];

const STATES_SOURCE_COLUMNS: &[PinnedSourceColumn] = &[
    pinned_source_column!("id", Integer, false, "integer"),
    pinned_source_column!("car_id", SmallInt, false, "smallint"),
    pinned_source_column!("state", StateStatus, false, "states_status"),
    pinned_source_column!(
        "start_date",
        Timestamp,
        false,
        "timestamp without time zone"
    ),
    pinned_source_column!("end_date", Timestamp, true, "timestamp without time zone"),
];

const UPDATES_SOURCE_COLUMNS: &[PinnedSourceColumn] = &[
    pinned_source_column!("id", Integer, false, "integer"),
    pinned_source_column!("car_id", SmallInt, false, "smallint"),
    pinned_source_column!(
        "start_date",
        Timestamp,
        false,
        "timestamp without time zone"
    ),
    pinned_source_column!("end_date", Timestamp, true, "timestamp without time zone"),
    pinned_source_column!("version", Varchar, true, "character varying(255)"),
];

// The complete physical shape of referenced address and geofence rows is part
// of the pinned source contract. `addresses.raw` is deliberately schema-probed
// only: the reader never selects, decodes, fingerprints, or publishes it.
const ADDRESS_SOURCE_COLUMNS: &[PinnedSourceColumn] = &[
    PinnedSourceColumn {
        name: "id",
        value_type: ValueType::Integer,
        nullable: false,
        format_type: "integer",
    },
    PinnedSourceColumn {
        name: "display_name",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(512)",
    },
    PinnedSourceColumn {
        name: "latitude",
        value_type: ValueType::Numeric,
        nullable: true,
        format_type: "numeric(8,6)",
    },
    PinnedSourceColumn {
        name: "longitude",
        value_type: ValueType::Numeric,
        nullable: true,
        format_type: "numeric(9,6)",
    },
    PinnedSourceColumn {
        name: "name",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "house_number",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "road",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "neighbourhood",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "city",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "county",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "postcode",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "state",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "state_district",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "country",
        value_type: ValueType::Varchar,
        nullable: true,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "raw",
        value_type: ValueType::Jsonb,
        nullable: true,
        format_type: "jsonb",
    },
    PinnedSourceColumn {
        name: "inserted_at",
        value_type: ValueType::Timestamp,
        nullable: false,
        format_type: "timestamp(0) without time zone",
    },
    PinnedSourceColumn {
        name: "updated_at",
        value_type: ValueType::Timestamp,
        nullable: false,
        format_type: "timestamp(0) without time zone",
    },
    PinnedSourceColumn {
        name: "osm_id",
        value_type: ValueType::BigInt,
        nullable: true,
        format_type: "bigint",
    },
    PinnedSourceColumn {
        name: "osm_type",
        value_type: ValueType::Text,
        nullable: true,
        format_type: "text",
    },
];

const GEOFENCE_SOURCE_COLUMNS: &[PinnedSourceColumn] = &[
    PinnedSourceColumn {
        name: "id",
        value_type: ValueType::Integer,
        nullable: false,
        format_type: "integer",
    },
    PinnedSourceColumn {
        name: "name",
        value_type: ValueType::Varchar,
        nullable: false,
        format_type: "character varying(255)",
    },
    PinnedSourceColumn {
        name: "latitude",
        value_type: ValueType::Numeric,
        nullable: false,
        format_type: "numeric(8,6)",
    },
    PinnedSourceColumn {
        name: "longitude",
        value_type: ValueType::Numeric,
        nullable: false,
        format_type: "numeric(9,6)",
    },
    PinnedSourceColumn {
        name: "radius",
        value_type: ValueType::SmallInt,
        nullable: false,
        format_type: "smallint",
    },
    PinnedSourceColumn {
        name: "billing_type",
        value_type: ValueType::BillingType,
        nullable: false,
        format_type: "billing_type",
    },
    PinnedSourceColumn {
        name: "cost_per_unit",
        value_type: ValueType::Numeric,
        nullable: true,
        format_type: "numeric(9,4)",
    },
    PinnedSourceColumn {
        name: "session_fee",
        value_type: ValueType::Numeric,
        nullable: true,
        format_type: "numeric(14,2)",
    },
    PinnedSourceColumn {
        name: "inserted_at",
        value_type: ValueType::Timestamp,
        nullable: false,
        format_type: "timestamp(0) without time zone",
    },
    PinnedSourceColumn {
        name: "updated_at",
        value_type: ValueType::Timestamp,
        nullable: false,
        format_type: "timestamp(0) without time zone",
    },
];

const PINNED_SOURCE_TABLES: &[PinnedSourceTable] = &[
    PinnedSourceTable {
        table: SourceTable::Cars,
        columns: CARS_SOURCE_COLUMNS,
    },
    PinnedSourceTable {
        table: SourceTable::CarSettings,
        columns: CAR_SETTINGS_SOURCE_COLUMNS,
    },
    PinnedSourceTable {
        table: SourceTable::Settings,
        columns: SETTINGS_SOURCE_COLUMNS,
    },
    PinnedSourceTable {
        table: SourceTable::Drives,
        columns: DRIVES_SOURCE_COLUMNS,
    },
    PinnedSourceTable {
        table: SourceTable::Positions,
        columns: POSITIONS_SOURCE_COLUMNS,
    },
    PinnedSourceTable {
        table: SourceTable::ChargingProcesses,
        columns: CHARGING_PROCESSES_SOURCE_COLUMNS,
    },
    PinnedSourceTable {
        table: SourceTable::Charges,
        columns: CHARGES_SOURCE_COLUMNS,
    },
    PinnedSourceTable {
        table: SourceTable::Addresses,
        columns: ADDRESS_SOURCE_COLUMNS,
    },
    PinnedSourceTable {
        table: SourceTable::Geofences,
        columns: GEOFENCE_SOURCE_COLUMNS,
    },
    PinnedSourceTable {
        table: SourceTable::States,
        columns: STATES_SOURCE_COLUMNS,
    },
    PinnedSourceTable {
        table: SourceTable::Updates,
        columns: UPDATES_SOURCE_COLUMNS,
    },
];

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
            source_table: None,
            source_name: $name,
            output_name: $name,
            value_type: ValueType::$type,
            nullable: $nullable,
        }
    };
}

macro_rules! settings_column {
    ($name:literal, $type:ident, $nullable:expr) => {
        ProjectionColumn {
            source_table: Some(SourceTable::CarSettings),
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
    settings_column!("suspend_min", SmallInt, true),
    settings_column!("suspend_after_idle_min", SmallInt, true),
    settings_column!("req_not_unlocked", Boolean, true),
    settings_column!("free_supercharging", Boolean, true),
    settings_column!("use_streaming_api", Boolean, true),
    settings_column!("enabled", Boolean, true),
    settings_column!("lfp_battery", Boolean, true),
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
  "settings"."suspend_min"::integer AS "suspend_min",
  "settings"."suspend_after_idle_min"::integer AS "suspend_after_idle_min",
  "settings"."req_not_unlocked" AS "req_not_unlocked",
  "settings"."free_supercharging" AS "free_supercharging",
  "settings"."use_streaming_api" AS "use_streaming_api",
  "settings"."enabled" AS "enabled",
  "settings"."lfp_battery" AS "lfp_battery",
  "source"."trim_badging" AS "trim_badging",
  "source"."marketing_name" AS "marketing_name",
  "source"."exterior_color" AS "exterior_color",
  "source"."wheel_type" AS "wheel_type",
  "source"."spoiler_type" AS "spoiler_type",
  "source"."display_priority" AS "display_priority",
  "source"."inserted_at" AS "inserted_at",
  "source"."updated_at" AS "updated_at"
FROM "public"."cars" AS "source"
JOIN "public"."car_settings" AS "settings"
  ON "settings"."id" = "source"."settings_id"
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
  "source"."id"::integer AS "id",
  "source"."car_id"::smallint AS "car_id",
  "source"."drive_id"::bigint AS "drive_id",
  "source"."date" AS "date",
  "source"."latitude"::numeric AS "latitude",
  "source"."longitude"::numeric AS "longitude",
  "source"."elevation"::bigint AS "elevation",
  "source"."speed"::bigint AS "speed",
  "source"."power"::double precision AS "power",
  "source"."odometer"::double precision AS "odometer",
  "source"."ideal_battery_range_km"::numeric AS "ideal_battery_range_km",
  "source"."est_battery_range_km"::numeric AS "est_battery_range_km",
  "source"."rated_battery_range_km"::numeric AS "rated_battery_range_km",
  "source"."battery_level"::bigint AS "battery_level",
  "source"."usable_battery_level"::bigint AS "usable_battery_level",
  "source"."battery_heater" AS "battery_heater",
  "source"."battery_heater_on" AS "battery_heater_on",
  "source"."battery_heater_no_power" AS "battery_heater_no_power",
  "source"."outside_temp"::numeric AS "outside_temp",
  "source"."inside_temp"::numeric AS "inside_temp",
  "source"."fan_status"::bigint AS "fan_status",
  "source"."driver_temp_setting"::numeric AS "driver_temp_setting",
  "source"."passenger_temp_setting"::numeric AS "passenger_temp_setting",
  "source"."is_climate_on" AS "is_climate_on",
  "source"."is_rear_defroster_on" AS "is_rear_defroster_on",
  "source"."is_front_defroster_on" AS "is_front_defroster_on",
  "source"."tpms_pressure_fl"::numeric AS "tpms_pressure_fl",
  "source"."tpms_pressure_fr"::numeric AS "tpms_pressure_fr",
  "source"."tpms_pressure_rl"::numeric AS "tpms_pressure_rl",
  "source"."tpms_pressure_rr"::numeric AS "tpms_pressure_rr"
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
        SourceTable::CarSettings => panic!("car_settings is a joined source relation"),
        SourceTable::Settings => panic!("settings is a singleton schema-probe relation"),
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
    pub format_type: &'a str,
    pub nullable: bool,
}

/// One ordered enum label returned by [`ENUM_PROBE_SQL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedEnumLabel<'a> {
    pub type_name: &'a str,
    pub label: &'a str,
}

/// A conservative reason a source schema cannot be read by this adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SchemaCompatibilityError {
    #[error("TeslaMate migration {found} is older than supported migration {minimum}")]
    LegacyMigration { found: i64, minimum: i64 },
    #[error("TeslaMate migration {found} is newer than reviewed migration {maximum}")]
    UnreviewedMigration { found: i64, maximum: i64 },
    #[error("TeslaMate migration set is missing or not strictly ordered")]
    InvalidMigrationSet,
    #[error("TeslaMate migration set does not match the pinned source revision")]
    MigrationSetMismatch,
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
    #[error("TeslaMate source column {table}.{column} has incompatible nullability")]
    IncompatibleColumnNullability {
        table: &'static str,
        column: &'static str,
    },
    #[error("TeslaMate source column {table}.{column} has incompatible physical format")]
    IncompatibleColumnFormat {
        table: &'static str,
        column: &'static str,
    },
    #[error("TeslaMate enum {type_name} does not match the pinned label set")]
    EnumLabelsMismatch { type_name: &'static str },
    #[error("TeslaMate global settings/cardinality relationship is invalid")]
    InvalidSettingsRelationship,
}

/// Reject a source high-water mark outside the pinned migration revision. The
/// reader also calls [`validate_migration_versions`] to require the exact
/// ordered migration set inside its read-only repeatable-read transaction.
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

/// Validate the complete installed migration set against the reviewed upstream
/// source revision. A matching `MAX(version)` alone is insufficient: a source
/// with a removed, substituted, or duplicate migration could otherwise enter
/// the typed reader under an accidental schema collision.
pub fn validate_migration_versions(versions: &[i64]) -> Result<i64, SchemaCompatibilityError> {
    let Some(&high_water) = versions.last() else {
        return Err(SchemaCompatibilityError::InvalidMigrationSet);
    };
    if versions.len() != TESLAMATE_V4_MIGRATION_COUNT {
        return Err(SchemaCompatibilityError::MigrationSetMismatch);
    }
    if versions
        .windows(2)
        .any(|pair| pair[0] <= 0 || pair[1] <= pair[0])
        || versions[0] <= 0
    {
        return Err(SchemaCompatibilityError::InvalidMigrationSet);
    }
    validate_migration_version(high_water)?;

    let mut digest = Sha256::new();
    for version in versions {
        digest.update(format!("{version:014}\n").as_bytes());
    }
    if hex::encode(digest.finalize()) != TESLAMATE_V4_MIGRATION_SET_SHA256 {
        return Err(SchemaCompatibilityError::MigrationSetMismatch);
    }
    Ok(high_water)
}

/// Validate the source metadata returned by [`SCHEMA_PROBE_SQL`]. Extra
/// tables and columns are allowed; every pinned physical field must retain its
/// exact PostgreSQL format and nullability. The reader projections stay
/// narrower than this admission contract.
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
            let source_table = expected_column.source_table.unwrap_or(table);
            let Some(actual) = observed.iter().find(|column| {
                column.table == source_table.name() && column.name == expected_column.source_name
            }) else {
                return Err(SchemaCompatibilityError::MissingColumn {
                    table: source_table.name(),
                    column: expected_column.source_name,
                });
            };

            let compatible_type = if source_table == SourceTable::CarSettings
                && expected_column.value_type == ValueType::SmallInt
            {
                matches!(actual.type_name, "int2" | "int4")
            } else {
                expected_column.value_type.accepts_udt(actual.type_name)
            };
            if !compatible_type {
                return Err(SchemaCompatibilityError::IncompatibleColumnType {
                    table: source_table.name(),
                    column: expected_column.source_name,
                    expected: expected_column.value_type,
                });
            }
        }
    }

    for table in PINNED_SOURCE_TABLES {
        for expected in table.columns {
            let Some(actual) = observed
                .iter()
                .find(|column| column.table == table.table.name() && column.name == expected.name)
            else {
                return Err(SchemaCompatibilityError::MissingColumn {
                    table: table.table.name(),
                    column: expected.name,
                });
            };
            if !expected.value_type.accepts_udt(actual.type_name) {
                return Err(SchemaCompatibilityError::IncompatibleColumnType {
                    table: table.table.name(),
                    column: expected.name,
                    expected: expected.value_type,
                });
            }
            if actual.nullable != expected.nullable {
                return Err(SchemaCompatibilityError::IncompatibleColumnNullability {
                    table: table.table.name(),
                    column: expected.name,
                });
            }
            if actual.format_type != expected.format_type {
                return Err(SchemaCompatibilityError::IncompatibleColumnFormat {
                    table: table.table.name(),
                    column: expected.name,
                });
            }
        }
    }

    Ok(())
}

const PINNED_ENUM_LABELS: &[(&str, &[&str])] = &[
    ("billing_type", &["per_kwh", "per_minute"]),
    ("range", &["ideal", "rated"]),
    ("states_status", &["online", "offline", "asleep"]),
    ("unit_of_length", &["km", "mi"]),
    ("unit_of_pressure", &["bar", "psi"]),
    ("unit_of_temperature", &["C", "F"]),
];

/// Require each pinned source enum to have the exact labels in the exact
/// upstream order. This prevents a familiar type name from masking a changed
/// availability or settings vocabulary.
pub fn validate_observed_enums(
    observed: &[ObservedEnumLabel<'_>],
) -> Result<(), SchemaCompatibilityError> {
    for (type_name, expected_labels) in PINNED_ENUM_LABELS {
        let labels: Vec<_> = observed
            .iter()
            .filter(|observed| observed.type_name == *type_name)
            .map(|observed| observed.label)
            .collect();
        if labels.as_slice() != *expected_labels {
            return Err(SchemaCompatibilityError::EnumLabelsMismatch { type_name });
        }
    }
    Ok(())
}

/// Validate the fixed global-settings singleton and required car-settings
/// relationship returned by [`SETTINGS_RELATIONSHIP_SQL`]. Orphaned historical
/// settings are allowed by the upstream source foreign-key direction.
pub const fn validate_settings_relationship(
    settings_count: i64,
    cars_without_settings: i64,
) -> Result<(), SchemaCompatibilityError> {
    if settings_count != 1 || cars_without_settings != 0 {
        return Err(SchemaCompatibilityError::InvalidSettingsRelationship);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PINNED_MIGRATION_VERSIONS: [i64; TESLAMATE_V4_MIGRATION_COUNT] = [
        20190330150000,
        20190330160000,
        20190330170000,
        20190330180000,
        20190330190000,
        20190330200000,
        20190408203117,
        20190415103933,
        20190415115227,
        20190415130006,
        20190415130705,
        20190415192200,
        20190416125429,
        20190525125700,
        20190717184003,
        20190729142656,
        20190729181314,
        20190730101523,
        20190731154452,
        20190805092941,
        20190810105216,
        20190810131321,
        20190810151901,
        20190812191616,
        20190813184320,
        20190814152810,
        20190816165713,
        20190816200723,
        20190821143938,
        20190821155748,
        20190823173437,
        20190826142828,
        20190828094708,
        20190828104902,
        20190828122529,
        20190828150058,
        20190903151524,
        20190913165850,
        20190913175011,
        20190913175543,
        20190925152807,
        20190925161034,
        20190925182253,
        20190928155641,
        20191003130650,
        20191003132415,
        20191007105010,
        20191008191431,
        20191017003836,
        20191020130234,
        20191026144449,
        20191026145925,
        20191026185642,
        20191117042320,
        20191117143038,
        20191117171307,
        20191119162847,
        20191212215130,
        20191212230527,
        20200103073606,
        20200116190926,
        20200120130125,
        20200120142602,
        20200203120311,
        20200203180529,
        20200212001245,
        20200216121330,
        20200302100654,
        20200306130218,
        20200306133847,
        20200318164021,
        20200320140020,
        20200401170940,
        20200401171402,
        20200401171923,
        20200410112005,
        20200502140646,
        20200528163852,
        20200528173223,
        20200528175158,
        20200709165119,
        20210130174838,
        20210812173700,
        20210831153305,
        20211022103654,
        20220123131732,
        20220422132017,
        20220617170400,
        20220718085412,
        20230417225712,
        20240603152807,
        20240627021414,
        20240915193446,
        20240929084639,
        20250407155134,
        20250613133700,
        20250924215353,
        20251207212310,
        20251225150000,
        20260411070212,
        20260715081000,
        20260716110000,
        20260718160000,
        20260807090000,
        20260808090000,
    ];

    fn complete_schema() -> Vec<ObservedColumn<'static>> {
        let mut result = Vec::new();
        for table in SourceTable::ALL {
            for column in projection(table).columns {
                result.push(ObservedColumn {
                    table: column.source_table.unwrap_or(table).name(),
                    name: column.source_name,
                    type_name: column.value_type.canonical_udt(),
                    format_type: column.value_type.canonical_udt(),
                    nullable: column.nullable,
                });
            }
        }
        for table in PINNED_SOURCE_TABLES {
            for column in table.columns {
                if let Some(existing) = result.iter_mut().find(|existing| {
                    existing.table == table.table.name() && existing.name == column.name
                }) {
                    existing.type_name = column.value_type.canonical_udt();
                    existing.format_type = column.format_type;
                    existing.nullable = column.nullable;
                } else {
                    result.push(ObservedColumn {
                        table: table.table.name(),
                        name: column.name,
                        type_name: column.value_type.canonical_udt(),
                        format_type: column.format_type,
                        nullable: column.nullable,
                    });
                }
            }
        }
        result
    }

    #[test]
    fn all_queries_are_fixed_qualified_and_keyset_paginated() {
        assert!(MIGRATION_VERSION_SQL.contains("\"public\".\"schema_migrations\""));
        assert!(MIGRATION_VERSIONS_SQL.contains("\"public\".\"schema_migrations\""));
        assert!(MIGRATION_VERSIONS_SQL.contains("ORDER BY \"migration\".\"version\" ASC"));
        assert!(SCHEMA_PROBE_SQL.contains("\"pg_catalog\".\"pg_class\""));
        assert!(SCHEMA_PROBE_SQL.contains("\"pg_catalog\".\"pg_attribute\""));
        assert!(SCHEMA_PROBE_SQL.contains("'settings'"));
        assert!(SCHEMA_PROBE_SQL.contains("\"format_type\""));
        assert!(ENUM_PROBE_SQL.contains("\"pg_catalog\".\"pg_enum\""));
        assert!(ENUM_PROBE_SQL.contains("'billing_type'"));
        assert!(SETTINGS_RELATIONSHIP_SQL.contains("\"public\".\"settings\""));

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
                let source_alias = if column.source_table == Some(SourceTable::CarSettings) {
                    "settings"
                } else {
                    "source"
                };
                let expected_expression = if column.source_table == Some(SourceTable::CarSettings)
                    && column.value_type == ValueType::SmallInt
                {
                    format!(
                        "\"{}\".\"{}\"::integer AS \"{}\"",
                        source_alias, column.source_name, column.output_name
                    )
                } else if table == SourceTable::Positions {
                    let cast_type = if column.source_name == "power" {
                        Some("double precision")
                    } else {
                        match column.value_type {
                            ValueType::Integer => {
                                if table == SourceTable::Positions
                                    && matches!(column.source_name, "drive_id" | "fan_status")
                                {
                                    Some("bigint")
                                } else {
                                    Some("integer")
                                }
                            }
                            ValueType::SmallInt => {
                                if table == SourceTable::Positions
                                    && matches!(
                                        column.source_name,
                                        "elevation"
                                            | "speed"
                                            | "battery_level"
                                            | "usable_battery_level"
                                    )
                                {
                                    Some("bigint")
                                } else {
                                    Some("smallint")
                                }
                            }
                            ValueType::Numeric => Some("numeric"),
                            ValueType::Floating => Some("double precision"),
                            _ => None,
                        }
                    };
                    if let Some(cast_type) = cast_type {
                        format!(
                            "\"{}\".\"{}\"::{} AS \"{}\"",
                            source_alias, column.source_name, cast_type, column.output_name
                        )
                    } else {
                        format!(
                            "\"{}\".\"{}\" AS \"{}\"",
                            source_alias, column.source_name, column.output_name
                        )
                    }
                } else {
                    format!(
                        "\"{}\".\"{}\" AS \"{}\"",
                        source_alias, column.source_name, column.output_name
                    )
                };
                assert!(descriptor.sql.contains(&expected_expression));
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
        assert_eq!(projection(SourceTable::Cars).columns.len(), 22);
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
    fn reviewed_migration_pin_requires_the_exact_upstream_set() {
        assert_eq!(
            validate_migration_versions(&PINNED_MIGRATION_VERSIONS),
            Ok(MAX_VALIDATED_MIGRATION)
        );
        assert_eq!(TESLAMATE_V4_SOURCE_REVISION.len(), 40);
        assert_eq!(
            validate_migration_versions(&PINNED_MIGRATION_VERSIONS[1..]),
            Err(SchemaCompatibilityError::MigrationSetMismatch)
        );
        let mut duplicate = PINNED_MIGRATION_VERSIONS.to_vec();
        duplicate[10] = duplicate[9];
        assert_eq!(
            validate_migration_versions(&duplicate),
            Err(SchemaCompatibilityError::InvalidMigrationSet)
        );
        let mut substituted = PINNED_MIGRATION_VERSIONS.to_vec();
        substituted[10] += 1;
        assert_eq!(
            validate_migration_versions(&substituted),
            Err(SchemaCompatibilityError::MigrationSetMismatch)
        );
    }

    #[test]
    fn valid_current_schema_is_accepted() {
        assert_eq!(validate_observed_schema(&complete_schema()), Ok(()));
    }

    #[test]
    fn every_pinned_physical_column_rejects_an_incompatible_type_name() {
        let mut tested = 0;
        for table in PINNED_SOURCE_TABLES {
            for expected in table.columns {
                let mut incompatible = complete_schema();
                incompatible
                    .iter_mut()
                    .find(|column| {
                        column.table == table.table.name() && column.name == expected.name
                    })
                    .expect("complete physical field")
                    .type_name = "bytea";
                assert!(matches!(
                    validate_observed_schema(&incompatible),
                    Err(SchemaCompatibilityError::IncompatibleColumnType {
                        table: actual_table,
                        column: actual_column,
                        ..
                    }) if actual_table == table.table.name() && actual_column == expected.name
                ));
                tested += 1;
            }
        }
        assert_eq!(tested, 169);
    }

    #[test]
    fn telemetry_timestamps_retain_the_untyped_source_format() {
        let timestamps: Vec<_> = PINNED_SOURCE_TABLES
            .iter()
            .flat_map(|table| {
                table.columns.iter().filter_map(move |column| {
                    (column.format_type == "timestamp without time zone")
                        .then_some((table.table.name(), column.name))
                })
            })
            .collect();
        assert_eq!(
            timestamps,
            [
                ("drives", "start_date"),
                ("drives", "end_date"),
                ("positions", "date"),
                ("charging_processes", "start_date"),
                ("charging_processes", "end_date"),
                ("charges", "date"),
                ("states", "start_date"),
                ("states", "end_date"),
                ("updates", "start_date"),
                ("updates", "end_date"),
            ]
        );
    }

    fn complete_enums() -> Vec<ObservedEnumLabel<'static>> {
        PINNED_ENUM_LABELS
            .iter()
            .flat_map(|(type_name, labels)| {
                labels
                    .iter()
                    .map(move |label| ObservedEnumLabel { type_name, label })
            })
            .collect()
    }

    #[test]
    fn pinned_settings_and_car_settings_physical_contract_is_enforced() {
        let mut wrong_nullability = complete_schema();
        wrong_nullability
            .iter_mut()
            .find(|column| column.table == "settings" && column.name == "language")
            .expect("settings.language")
            .nullable = true;
        assert_eq!(
            validate_observed_schema(&wrong_nullability),
            Err(SchemaCompatibilityError::IncompatibleColumnNullability {
                table: "settings",
                column: "language",
            })
        );

        let mut wrong_format = complete_schema();
        wrong_format
            .iter_mut()
            .find(|column| column.table == "settings" && column.name == "base_url")
            .expect("settings.base_url")
            .format_type = "text";
        assert_eq!(
            validate_observed_schema(&wrong_format),
            Err(SchemaCompatibilityError::IncompatibleColumnFormat {
                table: "settings",
                column: "base_url",
            })
        );

        let mut wrong_settings_id = complete_schema();
        wrong_settings_id
            .iter_mut()
            .find(|column| column.table == "cars" && column.name == "settings_id")
            .expect("cars.settings_id")
            .type_name = "int4";
        assert_eq!(
            validate_observed_schema(&wrong_settings_id),
            Err(SchemaCompatibilityError::IncompatibleColumnType {
                table: "cars",
                column: "settings_id",
                expected: ValueType::BigInt,
            })
        );
    }

    #[test]
    fn selected_car_physical_columns_fail_closed_on_every_contract_dimension() {
        let tables = [
            (SourceTable::Cars, CARS_SOURCE_COLUMNS),
            (SourceTable::Drives, DRIVES_SOURCE_COLUMNS),
            (SourceTable::Positions, POSITIONS_SOURCE_COLUMNS),
            (
                SourceTable::ChargingProcesses,
                CHARGING_PROCESSES_SOURCE_COLUMNS,
            ),
            (SourceTable::Charges, CHARGES_SOURCE_COLUMNS),
            (SourceTable::States, STATES_SOURCE_COLUMNS),
            (SourceTable::Updates, UPDATES_SOURCE_COLUMNS),
        ];

        assert_eq!(CARS_SOURCE_COLUMNS.len(), 16);
        assert_eq!(DRIVES_SOURCE_COLUMNS.len(), 25);
        assert_eq!(POSITIONS_SOURCE_COLUMNS.len(), 30);
        assert_eq!(CHARGING_PROCESSES_SOURCE_COLUMNS.len(), 18);
        assert_eq!(CHARGES_SOURCE_COLUMNS.len(), 22);
        assert_eq!(STATES_SOURCE_COLUMNS.len(), 5);
        assert_eq!(UPDATES_SOURCE_COLUMNS.len(), 5);
        assert_eq!(
            PINNED_SOURCE_TABLES
                .iter()
                .map(|table| table.columns.len())
                .sum::<usize>(),
            169
        );

        for (table, columns) in tables {
            for expected in columns {
                let mut missing = complete_schema();
                missing.retain(|column| {
                    !(column.table == table.name() && column.name == expected.name)
                });
                assert_eq!(
                    validate_observed_schema(&missing),
                    Err(SchemaCompatibilityError::MissingColumn {
                        table: table.name(),
                        column: expected.name,
                    })
                );

                let mut wrong_format = complete_schema();
                wrong_format
                    .iter_mut()
                    .find(|column| column.table == table.name() && column.name == expected.name)
                    .expect("complete selected-car physical field")
                    .format_type = "unreviewed source format";
                assert_eq!(
                    validate_observed_schema(&wrong_format),
                    Err(SchemaCompatibilityError::IncompatibleColumnFormat {
                        table: table.name(),
                        column: expected.name,
                    })
                );

                let mut wrong_nullability = complete_schema();
                wrong_nullability
                    .iter_mut()
                    .find(|column| column.table == table.name() && column.name == expected.name)
                    .expect("complete selected-car physical field")
                    .nullable = !expected.nullable;
                assert_eq!(
                    validate_observed_schema(&wrong_nullability),
                    Err(SchemaCompatibilityError::IncompatibleColumnNullability {
                        table: table.name(),
                        column: expected.name,
                    })
                );
            }
        }
    }

    #[test]
    fn complete_address_and_geofence_physical_columns_fail_closed() {
        assert_eq!(ADDRESS_SOURCE_COLUMNS.len(), 19);
        assert_eq!(GEOFENCE_SOURCE_COLUMNS.len(), 10);

        for (table, columns) in [
            (SourceTable::Addresses, ADDRESS_SOURCE_COLUMNS),
            (SourceTable::Geofences, GEOFENCE_SOURCE_COLUMNS),
        ] {
            for required in columns {
                let mut missing = complete_schema();
                missing.retain(|column| {
                    !(column.table == table.name() && column.name == required.name)
                });
                assert_eq!(
                    validate_observed_schema(&missing),
                    Err(SchemaCompatibilityError::MissingColumn {
                        table: table.name(),
                        column: required.name,
                    })
                );
            }
        }

        let mut wrong_format = complete_schema();
        wrong_format
            .iter_mut()
            .find(|column| column.table == "geofences" && column.name == "cost_per_unit")
            .expect("geofences.cost_per_unit")
            .format_type = "numeric(6,2)";
        assert_eq!(
            validate_observed_schema(&wrong_format),
            Err(SchemaCompatibilityError::IncompatibleColumnFormat {
                table: "geofences",
                column: "cost_per_unit",
            })
        );

        let mut wrong_nullability = complete_schema();
        wrong_nullability
            .iter_mut()
            .find(|column| column.table == "geofences" && column.name == "latitude")
            .expect("geofences.latitude")
            .nullable = true;
        assert_eq!(
            validate_observed_schema(&wrong_nullability),
            Err(SchemaCompatibilityError::IncompatibleColumnNullability {
                table: "geofences",
                column: "latitude",
            })
        );
    }

    #[test]
    fn unprojected_address_and_geofence_physical_columns_fail_closed() {
        assert!(
            ADDRESS_SOURCE_COLUMNS
                .iter()
                .any(|column| column.name == "raw" && column.value_type == ValueType::Jsonb)
        );
        assert!(
            !projection(SourceTable::Addresses)
                .sql
                .contains("\"source\".\"raw\"")
        );

        let mut raw_as_json = complete_schema();
        raw_as_json
            .iter_mut()
            .find(|column| column.table == "addresses" && column.name == "raw")
            .expect("addresses.raw")
            .type_name = "json";
        assert_eq!(
            validate_observed_schema(&raw_as_json),
            Err(SchemaCompatibilityError::IncompatibleColumnType {
                table: "addresses",
                column: "raw",
                expected: ValueType::Jsonb,
            })
        );

        let mut address_coordinate_precision = complete_schema();
        address_coordinate_precision
            .iter_mut()
            .find(|column| column.table == "addresses" && column.name == "longitude")
            .expect("addresses.longitude")
            .format_type = "numeric(8,6)";
        assert_eq!(
            validate_observed_schema(&address_coordinate_precision),
            Err(SchemaCompatibilityError::IncompatibleColumnFormat {
                table: "addresses",
                column: "longitude",
            })
        );

        let mut address_timestamp_nullability = complete_schema();
        address_timestamp_nullability
            .iter_mut()
            .find(|column| column.table == "addresses" && column.name == "updated_at")
            .expect("addresses.updated_at")
            .nullable = true;
        assert_eq!(
            validate_observed_schema(&address_timestamp_nullability),
            Err(SchemaCompatibilityError::IncompatibleColumnNullability {
                table: "addresses",
                column: "updated_at",
            })
        );

        let mut geofence_timestamp_precision = complete_schema();
        geofence_timestamp_precision
            .iter_mut()
            .find(|column| column.table == "geofences" && column.name == "inserted_at")
            .expect("geofences.inserted_at")
            .format_type = "timestamp without time zone";
        assert_eq!(
            validate_observed_schema(&geofence_timestamp_precision),
            Err(SchemaCompatibilityError::IncompatibleColumnFormat {
                table: "geofences",
                column: "inserted_at",
            })
        );
    }

    #[test]
    fn pinned_enums_and_global_settings_relationship_fail_closed() {
        assert_eq!(validate_observed_enums(&complete_enums()), Ok(()));
        let mut states = complete_enums();
        states
            .iter_mut()
            .find(|value| value.type_name == "states_status" && value.label == "offline")
            .expect("offline state")
            .label = "driving";
        assert_eq!(
            validate_observed_enums(&states),
            Err(SchemaCompatibilityError::EnumLabelsMismatch {
                type_name: "states_status",
            })
        );
        let mut billing = complete_enums();
        billing
            .iter_mut()
            .find(|value| value.type_name == "billing_type" && value.label == "per_minute")
            .expect("per-minute billing")
            .label = "per_session";
        assert_eq!(
            validate_observed_enums(&billing),
            Err(SchemaCompatibilityError::EnumLabelsMismatch {
                type_name: "billing_type",
            })
        );
        assert_eq!(validate_settings_relationship(1, 0), Ok(()));
        assert_eq!(
            validate_settings_relationship(2, 0),
            Err(SchemaCompatibilityError::InvalidSettingsRelationship)
        );
        assert_eq!(
            validate_settings_relationship(1, 1),
            Err(SchemaCompatibilityError::InvalidSettingsRelationship)
        );
    }

    #[test]
    fn joined_car_settings_columns_are_required_at_their_source_table() {
        let mut missing_settings_column = complete_schema();
        missing_settings_column
            .retain(|column| !(column.table == "car_settings" && column.name == "suspend_min"));
        assert_eq!(
            validate_observed_schema(&missing_settings_column),
            Err(SchemaCompatibilityError::MissingColumn {
                table: "car_settings",
                column: "suspend_min",
            })
        );
    }

    #[test]
    fn legacy_car_settings_integer_columns_accept_int4_but_not_unrelated_types() {
        let mut legacy_schema = complete_schema();
        for column in &mut legacy_schema {
            if column.table == "car_settings"
                && matches!(column.name, "suspend_min" | "suspend_after_idle_min")
            {
                column.type_name = "int4";
            }
        }
        assert_eq!(validate_observed_schema(&legacy_schema), Ok(()));

        let mut unrelated_type = legacy_schema;
        unrelated_type
            .iter_mut()
            .find(|column| column.table == "car_settings" && column.name == "suspend_min")
            .expect("suspend_min exists")
            .type_name = "int8";
        assert_eq!(
            validate_observed_schema(&unrelated_type),
            Err(SchemaCompatibilityError::IncompatibleColumnType {
                table: "car_settings",
                column: "suspend_min",
                expected: ValueType::SmallInt,
            })
        );
    }

    #[test]
    fn legacy_position_power_is_normalized_to_the_float_decoder_type() {
        let sql = projection(SourceTable::Positions).sql;
        assert!(sql.contains("\"source\".\"power\"::double precision AS \"power\""));
        assert!(sql.contains("\"source\".\"odometer\"::double precision AS \"odometer\""));
        assert!(sql.contains("\"source\".\"latitude\"::numeric AS \"latitude\""));
        assert!(sql.contains("\"source\".\"fan_status\"::bigint AS \"fan_status\""));
        assert!(sql.contains("\"source\".\"drive_id\"::bigint AS \"drive_id\""));
        assert!(sql.contains("\"source\".\"battery_level\"::bigint AS \"battery_level\""));
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
