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
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
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
use crate::teslamate_schema::{TESLAMATE_V4_MIGRATION_SET_SHA256, TESLAMATE_V4_SOURCE_REVISION};

// Schema 2.2 is a separate, full-snapshot-only protocol boundary.  Keep the
// public identity here with the writer types, while leaving the byte-pinned
// 2.0/2.1 writer path untouched.
pub use crate::protocol::HUB_PROJECTION_SCHEMA_V3;

const COMPRESSION_LEVEL: i32 = 4;
// Projection packs are immutable non-secret Hub data. The collector may
// publish them, while the API service must serve the same inode through the
// shared data group. Staging stays owner-private until verification succeeds.
const SHARED_IMMUTABLE_PACK_MODE: u32 = 0o640;
pub(crate) const MAX_TEXT_BYTES: usize = 16 * 1024;
const THP2_2_GLOBAL_SETTINGS_FIELD_COUNT: u64 = 11;
const THP2_2_CAR_SETTINGS_FIELD_COUNT: u64 = 8;
const THP2_2_CARS_FIELD_COUNT: u64 = 16;
const THP2_2_DRIVES_FIELD_COUNT: u64 = 25;
const THP2_2_POSITIONS_FIELD_COUNT: u64 = 30;
const THP2_2_CHARGING_PROCESSES_FIELD_COUNT: u64 = 18;
const THP2_2_CHARGES_FIELD_COUNT: u64 = 22;
const THP2_2_ADDRESS_FIELD_COUNT: u64 = 18;
const THP2_2_GEOFENCE_FIELD_COUNT: u64 = 10;
const THP2_2_STATES_FIELD_COUNT: u64 = 5;
const THP2_2_UPDATES_FIELD_COUNT: u64 = 5;
const THP2_2_MAPPED_FIELD_COUNT: u64 = THP2_2_GLOBAL_SETTINGS_FIELD_COUNT
    + THP2_2_CAR_SETTINGS_FIELD_COUNT
    + THP2_2_CARS_FIELD_COUNT
    + THP2_2_DRIVES_FIELD_COUNT
    + THP2_2_POSITIONS_FIELD_COUNT
    + THP2_2_CHARGING_PROCESSES_FIELD_COUNT
    + THP2_2_CHARGES_FIELD_COUNT
    + THP2_2_ADDRESS_FIELD_COUNT
    + THP2_2_GEOFENCE_FIELD_COUNT
    + THP2_2_STATES_FIELD_COUNT
    + THP2_2_UPDATES_FIELD_COUNT;
const THP2_2_UNRECONCILED_FIELD_COUNT: u64 = 1;
const THP2_2_GLOBAL_SETTINGS_SLICE_SHA256: &str =
    "74b5ae9d96793d34ec93c9cd21fdcf5996b63dcca9695fe3f296ff21a3107926";
const THP2_2_CAR_SETTINGS_SLICE_SHA256: &str =
    "e528749f76d15fb5e87ab43692455885d3249e989f53492ee34a6d5140824730";
const THP2_2_CARS_SLICE_SHA256: &str =
    "415e969442843f3e137070525d04c0d48bf282eecd025fc0343a88677ab96dc5";
const THP2_2_CARS_EFFICIENCY_ENCODING: &str = "ieee754_bits_be_blob";
const THP2_2_DRIVES_FLOAT_ENCODING: &str = "ieee754_bits_be_blob";
const THP2_2_POSITIONS_ODOMETER_ENCODING: &str = "ieee754_bits_be_blob";
const THP2_2_POSITIONS_RELATION_SCOPE: &str =
    "source_car_fk_rust_admission;source_drive_fk_omitted_cross_car_target";
const THP2_2_CHARGING_TRI_STATE_BOOL_ENCODING: &str = "sqlite_null_or_0_or_1";
const THP2_2_CHARGES_RELATION_SCOPE: &str =
    "charges_with_extant_selected_car_process;constraint_not_re_attested";
const THP2_2_FIXED_NUMERIC_ENCODING: &str = "finite_scaled_i64_or_nan";
const THP2_2_POSTGRES_TIMESTAMP_ENCODING: &str = "postgres_timestamp_binary_i64_us_since_2000";
const THP2_2_POSTGRES_TIMESTAMP_0_ENCODING: &str =
    "postgres_timestamp_binary_i64_us_since_2000_timestamp_0";
pub(crate) const POSTGRES_TIMESTAMP_FINITE_MIN_US: i64 = -211_813_488_000_000_000;
pub(crate) const POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US: i64 = 9_223_371_331_200_000_000;
const THP2_2_STATES_SLICE_SHA256: &str =
    "b333995c8dd9ae58b12c0f0cca8324446fa1bc41df2e7a97877983703fd35b8e";
const THP2_2_UPDATES_SLICE_SHA256: &str =
    "1026cbe3f8599d42557154860e9d3c18ba9ab56d43b33d60a3323a460ed31e34";
#[cfg(test)]
const THP2_2_GLOBAL_SETTINGS_SLICE_CONTRACT: &str = concat!(
    "teslatlas-thp2.2-settings-v1\n",
    "id:bigint:not_null\n",
    "unit_of_length:unit_of_length:not_null\n",
    "unit_of_temperature:unit_of_temperature:not_null\n",
    "unit_of_pressure:unit_of_pressure:not_null\n",
    "preferred_range:range:not_null\n",
    "base_url:character varying(255):nullable\n",
    "grafana_url:character varying(255):nullable\n",
    "language:text:not_null\n",
    "theme_mode:text:not_null\n",
    "inserted_at:timestamp(0) without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000_timestamp_0\n",
    "updated_at:timestamp(0) without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000_timestamp_0\n",
);
#[cfg(test)]
const THP2_2_CAR_SETTINGS_SLICE_CONTRACT: &str = concat!(
    "teslatlas-thp2.2-car-settings-v1\n",
    "id:bigint:not_null\n",
    "suspend_min:integer:not_null\n",
    "suspend_after_idle_min:integer:not_null\n",
    "req_not_unlocked:boolean:not_null\n",
    "free_supercharging:boolean:not_null\n",
    "use_streaming_api:boolean:not_null\n",
    "enabled:boolean:not_null\n",
    "lfp_battery:boolean:not_null\n",
);
#[cfg(test)]
const THP2_2_CARS_SLICE_CONTRACT: &str = concat!(
    "teslatlas-thp2.2-cars-v1\n",
    "id:smallint:not_null\n",
    "eid:bigint:not_null\n",
    "vid:bigint:not_null\n",
    "vin:text:not_null\n",
    "name:text:nullable\n",
    "model:character varying(255):nullable\n",
    "efficiency:double precision:nullable:ieee754_bits_be_blob\n",
    "trim_badging:text:nullable\n",
    "marketing_name:character varying(255):nullable\n",
    "exterior_color:text:nullable\n",
    "wheel_type:text:nullable\n",
    "spoiler_type:text:nullable\n",
    "display_priority:smallint:not_null\n",
    "inserted_at:timestamp(0) without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000_timestamp_0\n",
    "updated_at:timestamp(0) without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000_timestamp_0\n",
    "settings_id:bigint:not_null\n",
);
#[cfg(test)]
const THP2_2_STATES_SLICE_CONTRACT: &str = concat!(
    "teslatlas-thp2.2-states-v1\n",
    "id:integer:not_null\n",
    "car_id:smallint:not_null\n",
    "state:states_status:not_null\n",
    "start_date:timestamp without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000\n",
    "end_date:timestamp without time zone:nullable:postgres_timestamp_binary_i64_us_since_2000\n",
);
#[cfg(test)]
const THP2_2_UPDATES_SLICE_CONTRACT: &str = concat!(
    "teslatlas-thp2.2-updates-v1\n",
    "id:integer:not_null\n",
    "car_id:smallint:not_null\n",
    "start_date:timestamp without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000\n",
    "end_date:timestamp without time zone:nullable:postgres_timestamp_binary_i64_us_since_2000\n",
    "version:character varying(255):nullable\n",
);
const THP2_2_CAR_SETTINGS_SQLITE_DDL: &str = r#"
CREATE TABLE car_settings (
    id INTEGER PRIMARY KEY,
    suspend_min INTEGER NOT NULL CHECK(suspend_min BETWEEN -2147483648 AND 2147483647),
    suspend_after_idle_min INTEGER NOT NULL CHECK(suspend_after_idle_min BETWEEN -2147483648 AND 2147483647),
    req_not_unlocked INTEGER NOT NULL CHECK(req_not_unlocked IN (0, 1)),
    free_supercharging INTEGER NOT NULL CHECK(free_supercharging IN (0, 1)),
    use_streaming_api INTEGER NOT NULL CHECK(use_streaming_api IN (0, 1)),
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    lfp_battery INTEGER NOT NULL CHECK(lfp_battery IN (0, 1))
) STRICT, WITHOUT ROWID
"#;
const THP2_2_GLOBAL_SETTINGS_SQLITE_DDL: &str = r#"
CREATE TABLE global_settings (
    id INTEGER PRIMARY KEY,
    unit_of_length TEXT NOT NULL CHECK(unit_of_length IN ('km', 'mi')),
    unit_of_temperature TEXT NOT NULL CHECK(unit_of_temperature IN ('C', 'F')),
    unit_of_pressure TEXT NOT NULL CHECK(unit_of_pressure IN ('bar', 'psi')),
    preferred_range TEXT NOT NULL CHECK(preferred_range IN ('ideal', 'rated')),
    base_url TEXT CHECK(base_url IS NULL OR length(base_url) <= 255),
    grafana_url TEXT CHECK(grafana_url IS NULL OR length(grafana_url) <= 255),
    language TEXT NOT NULL CHECK(length(CAST(language AS BLOB)) <= 16384),
    theme_mode TEXT NOT NULL CHECK(length(CAST(theme_mode AS BLOB)) <= 16384),
    inserted_at_pg_us INTEGER NOT NULL CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0)),
    updated_at_pg_us INTEGER NOT NULL CHECK(updated_at_pg_us = (-9223372036854775807 - 1) OR updated_at_pg_us = 9223372036854775807 OR (updated_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND updated_at_pg_us % 1000000 = 0))
) STRICT, WITHOUT ROWID
"#;
const THP2_2_CARS_SQLITE_DDL: &str = r#"
CREATE TABLE cars (
    id INTEGER PRIMARY KEY CHECK(id BETWEEN -32768 AND 32767),
    eid INTEGER NOT NULL,
    vid INTEGER NOT NULL,
    vin TEXT,
    name TEXT,
    model TEXT CHECK(model IS NULL OR length(model) <= 255),
    efficiency BLOB CHECK(efficiency IS NULL OR length(efficiency) = 8),
    trim_badging TEXT,
    marketing_name TEXT CHECK(marketing_name IS NULL OR length(marketing_name) <= 255),
    exterior_color TEXT,
    wheel_type TEXT,
    spoiler_type TEXT,
    display_priority INTEGER NOT NULL CHECK(display_priority BETWEEN -32768 AND 32767),
    inserted_at_pg_us INTEGER NOT NULL CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0)),
    updated_at_pg_us INTEGER NOT NULL CHECK(updated_at_pg_us = (-9223372036854775807 - 1) OR updated_at_pg_us = 9223372036854775807 OR (updated_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND updated_at_pg_us % 1000000 = 0)),
    settings_id INTEGER NOT NULL UNIQUE REFERENCES car_settings(id)
) STRICT, WITHOUT ROWID
"#;
const THP2_2_DRIVES_SLICE_SHA256: &str =
    "61a03561426e886b49c72a98ce0e80ec95fd4f69a762d2e884d726e83f6a42d6";
const THP2_2_POSITIONS_SLICE_SHA256: &str =
    "8b699739dd76d25f62d74274cda9e6dfafc24c52f101c8418c08d2cc702b7a8b";
const THP2_2_CHARGING_PROCESSES_SLICE_SHA256: &str =
    "bd0b3f60721289da8dda83d80283c134560f0ae810b2f99ee20916590c2aa9f6";
const THP2_2_CHARGES_SLICE_SHA256: &str =
    "c8075db09941a15c2deeda010b2e001b78a3e5f9b4cded547f123fbbf5ab79da";
#[cfg(test)]
const THP2_2_DRIVES_SLICE_CONTRACT: &str = concat!(
    "teslatlas-thp2.2-drives-v1\n",
    "id:integer:not_null\n",
    "car_id:smallint:not_null\n",
    "start_date:timestamp without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000\n",
    "end_date:timestamp without time zone:nullable:postgres_timestamp_binary_i64_us_since_2000\n",
    "start_position_id:integer:nullable\n",
    "end_position_id:integer:nullable\n",
    "start_address_id:integer:nullable\n",
    "end_address_id:integer:nullable\n",
    "start_geofence_id:integer:nullable\n",
    "end_geofence_id:integer:nullable\n",
    "outside_temp_avg:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
    "inside_temp_avg:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
    "speed_max:smallint:nullable\n",
    "power_max:smallint:nullable\n",
    "power_min:smallint:nullable\n",
    "start_ideal_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "end_ideal_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "start_rated_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "end_rated_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "start_km:double precision:nullable:ieee754_bits_be_blob\n",
    "end_km:double precision:nullable:ieee754_bits_be_blob\n",
    "distance:double precision:nullable:ieee754_bits_be_blob\n",
    "duration_min:smallint:nullable\n",
    "ascent:smallint:nullable\n",
    "descent:smallint:nullable\n",
);
#[cfg(test)]
const THP2_2_POSITIONS_SLICE_CONTRACT: &str = concat!(
    "teslatlas-thp2.2-positions-v1\n",
    "id:integer:not_null\n",
    "car_id:smallint:not_null\n",
    "drive_id:integer:nullable\n",
    "date:timestamp without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000\n",
    "latitude:numeric(8,6):not_null:finite_scaled_i64_or_nan:e6\n",
    "longitude:numeric(9,6):not_null:finite_scaled_i64_or_nan:e6\n",
    "elevation:smallint:nullable\n",
    "speed:smallint:nullable\n",
    "power:smallint:nullable\n",
    "odometer:double precision:nullable:ieee754_bits_be_blob\n",
    "ideal_battery_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "est_battery_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "rated_battery_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "battery_level:smallint:nullable\n",
    "usable_battery_level:smallint:nullable\n",
    "battery_heater:boolean:nullable\n",
    "battery_heater_on:boolean:nullable\n",
    "battery_heater_no_power:boolean:nullable\n",
    "outside_temp:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
    "inside_temp:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
    "fan_status:integer:nullable\n",
    "driver_temp_setting:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
    "passenger_temp_setting:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
    "is_climate_on:boolean:nullable\n",
    "is_rear_defroster_on:boolean:nullable\n",
    "is_front_defroster_on:boolean:nullable\n",
    "tpms_pressure_fl:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
    "tpms_pressure_fr:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
    "tpms_pressure_rl:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
    "tpms_pressure_rr:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
);
#[cfg(test)]
const THP2_2_CHARGING_PROCESSES_SLICE_CONTRACT: &str = concat!(
    "teslatlas-thp2.2-charging-processes-v1\n",
    "id:integer:not_null\n",
    "car_id:smallint:not_null\n",
    "position_id:integer:not_null\n",
    "address_id:integer:nullable\n",
    "geofence_id:integer:nullable\n",
    "start_date:timestamp without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000\n",
    "end_date:timestamp without time zone:nullable:postgres_timestamp_binary_i64_us_since_2000\n",
    "charge_energy_added:numeric(8,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "charge_energy_used:numeric(8,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "start_ideal_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "end_ideal_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "start_rated_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "end_rated_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "start_battery_level:smallint:nullable\n",
    "end_battery_level:smallint:nullable\n",
    "duration_min:smallint:nullable\n",
    "outside_temp_avg:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
    "cost:numeric(14,2):nullable:finite_scaled_i64_or_nan:e2\n",
);
#[cfg(test)]
const THP2_2_CHARGES_SLICE_CONTRACT: &str = concat!(
    "teslatlas-thp2.2-charges-v1\n",
    "id:integer:not_null\n",
    "charging_process_id:integer:not_null\n",
    "date:timestamp without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000\n",
    "battery_heater:boolean:nullable\n",
    "battery_heater_on:boolean:nullable\n",
    "battery_heater_no_power:boolean:nullable\n",
    "battery_level:smallint:nullable\n",
    "usable_battery_level:smallint:nullable\n",
    "charge_energy_added:numeric(8,2):not_null:finite_scaled_i64_or_nan:e2\n",
    "charger_actual_current:smallint:nullable\n",
    "charger_phases:smallint:nullable\n",
    "charger_pilot_current:smallint:nullable\n",
    "charger_power:smallint:not_null\n",
    "charger_voltage:smallint:nullable\n",
    "conn_charge_cable:character varying(255):nullable\n",
    "fast_charger_present:boolean:nullable\n",
    "fast_charger_brand:character varying(255):nullable\n",
    "fast_charger_type:character varying(255):nullable\n",
    "ideal_battery_range_km:numeric(6,2):not_null:finite_scaled_i64_or_nan:e2\n",
    "rated_battery_range_km:numeric(6,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "not_enough_power_to_heat:boolean:nullable\n",
    "outside_temp:numeric(4,1):nullable:finite_scaled_i64_or_nan:e1\n",
);
const THP2_2_CHARGING_PROCESSES_SQLITE_DDL: &str = r#"
CREATE TABLE charging_processes (
    id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
    car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767),
    position_id INTEGER NOT NULL CHECK(position_id BETWEEN -2147483648 AND 2147483647),
    address_id INTEGER CHECK(address_id IS NULL OR address_id BETWEEN -2147483648 AND 2147483647),
    geofence_id INTEGER CHECK(geofence_id IS NULL OR geofence_id BETWEEN -2147483648 AND 2147483647),
    start_date_pg_us INTEGER NOT NULL CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
    end_date_pg_us INTEGER CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
    charge_energy_added_e2 INTEGER,
    charge_energy_added_e2_is_nan INTEGER NOT NULL CHECK(charge_energy_added_e2_is_nan IN (0, 1)),
    charge_energy_used_e2 INTEGER,
    charge_energy_used_e2_is_nan INTEGER NOT NULL CHECK(charge_energy_used_e2_is_nan IN (0, 1)),
    start_ideal_range_km_e2 INTEGER,
    start_ideal_range_km_e2_is_nan INTEGER NOT NULL CHECK(start_ideal_range_km_e2_is_nan IN (0, 1)),
    end_ideal_range_km_e2 INTEGER,
    end_ideal_range_km_e2_is_nan INTEGER NOT NULL CHECK(end_ideal_range_km_e2_is_nan IN (0, 1)),
    start_rated_range_km_e2 INTEGER,
    start_rated_range_km_e2_is_nan INTEGER NOT NULL CHECK(start_rated_range_km_e2_is_nan IN (0, 1)),
    end_rated_range_km_e2 INTEGER,
    end_rated_range_km_e2_is_nan INTEGER NOT NULL CHECK(end_rated_range_km_e2_is_nan IN (0, 1)),
    start_battery_level INTEGER CHECK(start_battery_level IS NULL OR start_battery_level BETWEEN -32768 AND 32767),
    end_battery_level INTEGER CHECK(end_battery_level IS NULL OR end_battery_level BETWEEN -32768 AND 32767),
    duration_min INTEGER CHECK(duration_min IS NULL OR duration_min BETWEEN -32768 AND 32767),
    outside_temp_avg_e1 INTEGER,
    outside_temp_avg_e1_is_nan INTEGER NOT NULL CHECK(outside_temp_avg_e1_is_nan IN (0, 1)),
    cost_e2 INTEGER,
    cost_e2_is_nan INTEGER NOT NULL CHECK(cost_e2_is_nan IN (0, 1)),
    CHECK((charge_energy_added_e2 IS NULL AND charge_energy_added_e2_is_nan IN (0, 1)) OR (charge_energy_added_e2 IS NOT NULL AND charge_energy_added_e2_is_nan = 0 AND charge_energy_added_e2 BETWEEN -99999999 AND 99999999)),
    CHECK((charge_energy_used_e2 IS NULL AND charge_energy_used_e2_is_nan IN (0, 1)) OR (charge_energy_used_e2 IS NOT NULL AND charge_energy_used_e2_is_nan = 0 AND charge_energy_used_e2 BETWEEN -99999999 AND 99999999)),
    CHECK((start_ideal_range_km_e2 IS NULL AND start_ideal_range_km_e2_is_nan IN (0, 1)) OR (start_ideal_range_km_e2 IS NOT NULL AND start_ideal_range_km_e2_is_nan = 0 AND start_ideal_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((end_ideal_range_km_e2 IS NULL AND end_ideal_range_km_e2_is_nan IN (0, 1)) OR (end_ideal_range_km_e2 IS NOT NULL AND end_ideal_range_km_e2_is_nan = 0 AND end_ideal_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((start_rated_range_km_e2 IS NULL AND start_rated_range_km_e2_is_nan IN (0, 1)) OR (start_rated_range_km_e2 IS NOT NULL AND start_rated_range_km_e2_is_nan = 0 AND start_rated_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((end_rated_range_km_e2 IS NULL AND end_rated_range_km_e2_is_nan IN (0, 1)) OR (end_rated_range_km_e2 IS NOT NULL AND end_rated_range_km_e2_is_nan = 0 AND end_rated_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((outside_temp_avg_e1 IS NULL AND outside_temp_avg_e1_is_nan IN (0, 1)) OR (outside_temp_avg_e1 IS NOT NULL AND outside_temp_avg_e1_is_nan = 0 AND outside_temp_avg_e1 BETWEEN -9999 AND 9999)),
    CHECK((cost_e2 IS NULL AND cost_e2_is_nan IN (0, 1)) OR (cost_e2 IS NOT NULL AND cost_e2_is_nan = 0 AND cost_e2 BETWEEN -999999 AND 999999))
) STRICT, WITHOUT ROWID
"#;
const THP2_2_CHARGES_SQLITE_DDL: &str = r#"
CREATE TABLE charges (
    id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
    charging_process_id INTEGER NOT NULL CHECK(charging_process_id BETWEEN -2147483648 AND 2147483647),
    date_pg_us INTEGER NOT NULL CHECK(date_pg_us = (-9223372036854775807 - 1) OR date_pg_us = 9223372036854775807 OR date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
    battery_heater INTEGER CHECK(battery_heater IS NULL OR battery_heater IN (0, 1)),
    battery_heater_on INTEGER CHECK(battery_heater_on IS NULL OR battery_heater_on IN (0, 1)),
    battery_heater_no_power INTEGER CHECK(battery_heater_no_power IS NULL OR battery_heater_no_power IN (0, 1)),
    battery_level INTEGER CHECK(battery_level IS NULL OR battery_level BETWEEN -32768 AND 32767),
    usable_battery_level INTEGER CHECK(usable_battery_level IS NULL OR usable_battery_level BETWEEN -32768 AND 32767),
    charge_energy_added_e2 INTEGER,
    charge_energy_added_e2_is_nan INTEGER NOT NULL CHECK(charge_energy_added_e2_is_nan IN (0, 1)),
    charger_actual_current INTEGER CHECK(charger_actual_current IS NULL OR charger_actual_current BETWEEN -32768 AND 32767),
    charger_phases INTEGER CHECK(charger_phases IS NULL OR charger_phases BETWEEN -32768 AND 32767),
    charger_pilot_current INTEGER CHECK(charger_pilot_current IS NULL OR charger_pilot_current BETWEEN -32768 AND 32767),
    charger_power INTEGER NOT NULL CHECK(charger_power BETWEEN -32768 AND 32767),
    charger_voltage INTEGER CHECK(charger_voltage IS NULL OR charger_voltage BETWEEN -32768 AND 32767),
    conn_charge_cable TEXT CHECK(conn_charge_cable IS NULL OR length(conn_charge_cable) <= 255),
    fast_charger_present INTEGER CHECK(fast_charger_present IS NULL OR fast_charger_present IN (0, 1)),
    fast_charger_brand TEXT CHECK(fast_charger_brand IS NULL OR length(fast_charger_brand) <= 255),
    fast_charger_type TEXT CHECK(fast_charger_type IS NULL OR length(fast_charger_type) <= 255),
    ideal_battery_range_km_e2 INTEGER,
    ideal_battery_range_km_e2_is_nan INTEGER NOT NULL CHECK(ideal_battery_range_km_e2_is_nan IN (0, 1)),
    rated_battery_range_km_e2 INTEGER,
    rated_battery_range_km_e2_is_nan INTEGER NOT NULL CHECK(rated_battery_range_km_e2_is_nan IN (0, 1)),
    not_enough_power_to_heat INTEGER CHECK(not_enough_power_to_heat IS NULL OR not_enough_power_to_heat IN (0, 1)),
    outside_temp_e1 INTEGER,
    outside_temp_e1_is_nan INTEGER NOT NULL CHECK(outside_temp_e1_is_nan IN (0, 1)),
    CHECK((charge_energy_added_e2 IS NULL AND charge_energy_added_e2_is_nan = 1) OR (charge_energy_added_e2 IS NOT NULL AND charge_energy_added_e2_is_nan = 0 AND charge_energy_added_e2 BETWEEN -99999999 AND 99999999)),
    CHECK((ideal_battery_range_km_e2 IS NULL AND ideal_battery_range_km_e2_is_nan = 1) OR (ideal_battery_range_km_e2 IS NOT NULL AND ideal_battery_range_km_e2_is_nan = 0 AND ideal_battery_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((rated_battery_range_km_e2 IS NULL AND rated_battery_range_km_e2_is_nan IN (0, 1)) OR (rated_battery_range_km_e2 IS NOT NULL AND rated_battery_range_km_e2_is_nan = 0 AND rated_battery_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((outside_temp_e1 IS NULL AND outside_temp_e1_is_nan IN (0, 1)) OR (outside_temp_e1 IS NOT NULL AND outside_temp_e1_is_nan = 0 AND outside_temp_e1 BETWEEN -9999 AND 9999))
) STRICT, WITHOUT ROWID
"#;
const THP2_2_DRIVES_SQLITE_DDL: &str = r#"
CREATE TABLE drives (
    id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
    car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767),
    start_date_pg_us INTEGER NOT NULL CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
    end_date_pg_us INTEGER CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
    start_position_id INTEGER CHECK(start_position_id IS NULL OR start_position_id BETWEEN -2147483648 AND 2147483647),
    end_position_id INTEGER CHECK(end_position_id IS NULL OR end_position_id BETWEEN -2147483648 AND 2147483647),
    start_address_id INTEGER CHECK(start_address_id IS NULL OR start_address_id BETWEEN -2147483648 AND 2147483647),
    end_address_id INTEGER CHECK(end_address_id IS NULL OR end_address_id BETWEEN -2147483648 AND 2147483647),
    start_geofence_id INTEGER CHECK(start_geofence_id IS NULL OR start_geofence_id BETWEEN -2147483648 AND 2147483647),
    end_geofence_id INTEGER CHECK(end_geofence_id IS NULL OR end_geofence_id BETWEEN -2147483648 AND 2147483647),
    outside_temp_avg_e1 INTEGER,
    outside_temp_avg_e1_is_nan INTEGER NOT NULL CHECK(outside_temp_avg_e1_is_nan IN (0, 1)),
    inside_temp_avg_e1 INTEGER,
    inside_temp_avg_e1_is_nan INTEGER NOT NULL CHECK(inside_temp_avg_e1_is_nan IN (0, 1)),
    speed_max INTEGER CHECK(speed_max IS NULL OR speed_max BETWEEN -32768 AND 32767),
    power_max INTEGER CHECK(power_max IS NULL OR power_max BETWEEN -32768 AND 32767),
    power_min INTEGER CHECK(power_min IS NULL OR power_min BETWEEN -32768 AND 32767),
    start_ideal_range_km_e2 INTEGER,
    start_ideal_range_km_e2_is_nan INTEGER NOT NULL CHECK(start_ideal_range_km_e2_is_nan IN (0, 1)),
    end_ideal_range_km_e2 INTEGER,
    end_ideal_range_km_e2_is_nan INTEGER NOT NULL CHECK(end_ideal_range_km_e2_is_nan IN (0, 1)),
    start_rated_range_km_e2 INTEGER,
    start_rated_range_km_e2_is_nan INTEGER NOT NULL CHECK(start_rated_range_km_e2_is_nan IN (0, 1)),
    end_rated_range_km_e2 INTEGER,
    end_rated_range_km_e2_is_nan INTEGER NOT NULL CHECK(end_rated_range_km_e2_is_nan IN (0, 1)),
    start_km_f64_be BLOB CHECK(start_km_f64_be IS NULL OR length(start_km_f64_be) = 8),
    end_km_f64_be BLOB CHECK(end_km_f64_be IS NULL OR length(end_km_f64_be) = 8),
    distance_f64_be BLOB CHECK(distance_f64_be IS NULL OR length(distance_f64_be) = 8),
    duration_min INTEGER CHECK(duration_min IS NULL OR duration_min BETWEEN -32768 AND 32767),
    ascent INTEGER CHECK(ascent IS NULL OR ascent BETWEEN -32768 AND 32767),
    descent INTEGER CHECK(descent IS NULL OR descent BETWEEN -32768 AND 32767),
    CHECK((outside_temp_avg_e1 IS NULL AND outside_temp_avg_e1_is_nan IN (0, 1)) OR (outside_temp_avg_e1 IS NOT NULL AND outside_temp_avg_e1_is_nan = 0 AND outside_temp_avg_e1 BETWEEN -9999 AND 9999)),
    CHECK((inside_temp_avg_e1 IS NULL AND inside_temp_avg_e1_is_nan IN (0, 1)) OR (inside_temp_avg_e1 IS NOT NULL AND inside_temp_avg_e1_is_nan = 0 AND inside_temp_avg_e1 BETWEEN -9999 AND 9999)),
    CHECK((start_ideal_range_km_e2 IS NULL AND start_ideal_range_km_e2_is_nan IN (0, 1)) OR (start_ideal_range_km_e2 IS NOT NULL AND start_ideal_range_km_e2_is_nan = 0 AND start_ideal_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((end_ideal_range_km_e2 IS NULL AND end_ideal_range_km_e2_is_nan IN (0, 1)) OR (end_ideal_range_km_e2 IS NOT NULL AND end_ideal_range_km_e2_is_nan = 0 AND end_ideal_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((start_rated_range_km_e2 IS NULL AND start_rated_range_km_e2_is_nan IN (0, 1)) OR (start_rated_range_km_e2 IS NOT NULL AND start_rated_range_km_e2_is_nan = 0 AND start_rated_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((end_rated_range_km_e2 IS NULL AND end_rated_range_km_e2_is_nan IN (0, 1)) OR (end_rated_range_km_e2 IS NOT NULL AND end_rated_range_km_e2_is_nan = 0 AND end_rated_range_km_e2 BETWEEN -999999 AND 999999))
) STRICT, WITHOUT ROWID
"#;
const THP2_2_POSITIONS_SQLITE_DDL: &str = r#"
CREATE TABLE positions (
    id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
    car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767),
    drive_id INTEGER CHECK(drive_id IS NULL OR drive_id BETWEEN -2147483648 AND 2147483647),
    date_pg_us INTEGER NOT NULL CHECK(date_pg_us = (-9223372036854775807 - 1) OR date_pg_us = 9223372036854775807 OR date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
    latitude_e6 INTEGER,
    latitude_e6_is_nan INTEGER NOT NULL CHECK(latitude_e6_is_nan IN (0, 1)),
    longitude_e6 INTEGER,
    longitude_e6_is_nan INTEGER NOT NULL CHECK(longitude_e6_is_nan IN (0, 1)),
    elevation INTEGER CHECK(elevation IS NULL OR elevation BETWEEN -32768 AND 32767),
    speed INTEGER CHECK(speed IS NULL OR speed BETWEEN -32768 AND 32767),
    power INTEGER CHECK(power IS NULL OR power BETWEEN -32768 AND 32767),
    odometer_f64_be BLOB CHECK(odometer_f64_be IS NULL OR length(odometer_f64_be) = 8),
    ideal_battery_range_km_e2 INTEGER,
    ideal_battery_range_km_e2_is_nan INTEGER NOT NULL CHECK(ideal_battery_range_km_e2_is_nan IN (0, 1)),
    est_battery_range_km_e2 INTEGER,
    est_battery_range_km_e2_is_nan INTEGER NOT NULL CHECK(est_battery_range_km_e2_is_nan IN (0, 1)),
    rated_battery_range_km_e2 INTEGER,
    rated_battery_range_km_e2_is_nan INTEGER NOT NULL CHECK(rated_battery_range_km_e2_is_nan IN (0, 1)),
    battery_level INTEGER CHECK(battery_level IS NULL OR battery_level BETWEEN -32768 AND 32767),
    usable_battery_level INTEGER CHECK(usable_battery_level IS NULL OR usable_battery_level BETWEEN -32768 AND 32767),
    battery_heater INTEGER CHECK(battery_heater IS NULL OR battery_heater IN (0, 1)),
    battery_heater_on INTEGER CHECK(battery_heater_on IS NULL OR battery_heater_on IN (0, 1)),
    battery_heater_no_power INTEGER CHECK(battery_heater_no_power IS NULL OR battery_heater_no_power IN (0, 1)),
    outside_temp_e1 INTEGER,
    outside_temp_e1_is_nan INTEGER NOT NULL CHECK(outside_temp_e1_is_nan IN (0, 1)),
    inside_temp_e1 INTEGER,
    inside_temp_e1_is_nan INTEGER NOT NULL CHECK(inside_temp_e1_is_nan IN (0, 1)),
    fan_status INTEGER CHECK(fan_status IS NULL OR fan_status BETWEEN -2147483648 AND 2147483647),
    driver_temp_setting_e1 INTEGER,
    driver_temp_setting_e1_is_nan INTEGER NOT NULL CHECK(driver_temp_setting_e1_is_nan IN (0, 1)),
    passenger_temp_setting_e1 INTEGER,
    passenger_temp_setting_e1_is_nan INTEGER NOT NULL CHECK(passenger_temp_setting_e1_is_nan IN (0, 1)),
    is_climate_on INTEGER CHECK(is_climate_on IS NULL OR is_climate_on IN (0, 1)),
    is_rear_defroster_on INTEGER CHECK(is_rear_defroster_on IS NULL OR is_rear_defroster_on IN (0, 1)),
    is_front_defroster_on INTEGER CHECK(is_front_defroster_on IS NULL OR is_front_defroster_on IN (0, 1)),
    tpms_pressure_fl_e1 INTEGER,
    tpms_pressure_fl_e1_is_nan INTEGER NOT NULL CHECK(tpms_pressure_fl_e1_is_nan IN (0, 1)),
    tpms_pressure_fr_e1 INTEGER,
    tpms_pressure_fr_e1_is_nan INTEGER NOT NULL CHECK(tpms_pressure_fr_e1_is_nan IN (0, 1)),
    tpms_pressure_rl_e1 INTEGER,
    tpms_pressure_rl_e1_is_nan INTEGER NOT NULL CHECK(tpms_pressure_rl_e1_is_nan IN (0, 1)),
    tpms_pressure_rr_e1 INTEGER,
    tpms_pressure_rr_e1_is_nan INTEGER NOT NULL CHECK(tpms_pressure_rr_e1_is_nan IN (0, 1)),
    CHECK((latitude_e6 IS NULL AND latitude_e6_is_nan = 1) OR (latitude_e6 IS NOT NULL AND latitude_e6_is_nan = 0 AND latitude_e6 BETWEEN -99999999 AND 99999999)),
    CHECK((longitude_e6 IS NULL AND longitude_e6_is_nan = 1) OR (longitude_e6 IS NOT NULL AND longitude_e6_is_nan = 0 AND longitude_e6 BETWEEN -999999999 AND 999999999)),
    CHECK((ideal_battery_range_km_e2 IS NULL AND ideal_battery_range_km_e2_is_nan IN (0, 1)) OR (ideal_battery_range_km_e2 IS NOT NULL AND ideal_battery_range_km_e2_is_nan = 0 AND ideal_battery_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((est_battery_range_km_e2 IS NULL AND est_battery_range_km_e2_is_nan IN (0, 1)) OR (est_battery_range_km_e2 IS NOT NULL AND est_battery_range_km_e2_is_nan = 0 AND est_battery_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((rated_battery_range_km_e2 IS NULL AND rated_battery_range_km_e2_is_nan IN (0, 1)) OR (rated_battery_range_km_e2 IS NOT NULL AND rated_battery_range_km_e2_is_nan = 0 AND rated_battery_range_km_e2 BETWEEN -999999 AND 999999)),
    CHECK((outside_temp_e1 IS NULL AND outside_temp_e1_is_nan IN (0, 1)) OR (outside_temp_e1 IS NOT NULL AND outside_temp_e1_is_nan = 0 AND outside_temp_e1 BETWEEN -9999 AND 9999)),
    CHECK((inside_temp_e1 IS NULL AND inside_temp_e1_is_nan IN (0, 1)) OR (inside_temp_e1 IS NOT NULL AND inside_temp_e1_is_nan = 0 AND inside_temp_e1 BETWEEN -9999 AND 9999)),
    CHECK((driver_temp_setting_e1 IS NULL AND driver_temp_setting_e1_is_nan IN (0, 1)) OR (driver_temp_setting_e1 IS NOT NULL AND driver_temp_setting_e1_is_nan = 0 AND driver_temp_setting_e1 BETWEEN -9999 AND 9999)),
    CHECK((passenger_temp_setting_e1 IS NULL AND passenger_temp_setting_e1_is_nan IN (0, 1)) OR (passenger_temp_setting_e1 IS NOT NULL AND passenger_temp_setting_e1_is_nan = 0 AND passenger_temp_setting_e1 BETWEEN -9999 AND 9999)),
    CHECK((tpms_pressure_fl_e1 IS NULL AND tpms_pressure_fl_e1_is_nan IN (0, 1)) OR (tpms_pressure_fl_e1 IS NOT NULL AND tpms_pressure_fl_e1_is_nan = 0 AND tpms_pressure_fl_e1 BETWEEN -9999 AND 9999)),
    CHECK((tpms_pressure_fr_e1 IS NULL AND tpms_pressure_fr_e1_is_nan IN (0, 1)) OR (tpms_pressure_fr_e1 IS NOT NULL AND tpms_pressure_fr_e1_is_nan = 0 AND tpms_pressure_fr_e1 BETWEEN -9999 AND 9999)),
    CHECK((tpms_pressure_rl_e1 IS NULL AND tpms_pressure_rl_e1_is_nan IN (0, 1)) OR (tpms_pressure_rl_e1 IS NOT NULL AND tpms_pressure_rl_e1_is_nan = 0 AND tpms_pressure_rl_e1 BETWEEN -9999 AND 9999)),
    CHECK((tpms_pressure_rr_e1 IS NULL AND tpms_pressure_rr_e1_is_nan IN (0, 1)) OR (tpms_pressure_rr_e1 IS NOT NULL AND tpms_pressure_rr_e1_is_nan = 0 AND tpms_pressure_rr_e1 BETWEEN -9999 AND 9999))
) STRICT, WITHOUT ROWID
"#;
const THP2_2_STATES_SQLITE_DDL: &str = r#"
CREATE TABLE states (
    id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
    car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767) REFERENCES cars(id),
    state TEXT NOT NULL CHECK(state IN ('online', 'offline', 'asleep')),
    start_date_pg_us INTEGER NOT NULL CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
    end_date_pg_us INTEGER CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999)
) STRICT, WITHOUT ROWID
"#;
const THP2_2_UPDATES_SQLITE_DDL: &str = r#"
CREATE TABLE updates (
    id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
    car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767) REFERENCES cars(id),
    start_date_pg_us INTEGER NOT NULL CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
    end_date_pg_us INTEGER CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
    version TEXT CHECK(version IS NULL OR length(version) <= 255)
) STRICT, WITHOUT ROWID
"#;
const THP2_2_ADDRESS_SLICE_SHA256: &str =
    "7a8595ee2ee7c76f0573b2c3bac24f2551c708b1893af5734cbd6d770d6b1834";
#[cfg(test)]
const THP2_2_ADDRESS_SLICE_CONTRACT: &str = concat!(
    "teslatlas-thp2.2-addresses-v1\n",
    "id:integer:not_null\n",
    "display_name:character varying(512):nullable\n",
    "latitude:numeric(8,6):nullable:finite_scaled_i64_or_nan:e6\n",
    "longitude:numeric(9,6):nullable:finite_scaled_i64_or_nan:e6\n",
    "name:character varying(255):nullable\n",
    "house_number:character varying(255):nullable\n",
    "road:character varying(255):nullable\n",
    "neighbourhood:character varying(255):nullable\n",
    "city:character varying(255):nullable\n",
    "county:character varying(255):nullable\n",
    "postcode:character varying(255):nullable\n",
    "state:character varying(255):nullable\n",
    "state_district:character varying(255):nullable\n",
    "country:character varying(255):nullable\n",
    "inserted_at:timestamp(0) without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000_timestamp_0\n",
    "updated_at:timestamp(0) without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000_timestamp_0\n",
    "osm_id:bigint:nullable\n",
    "osm_type:text:nullable\n",
);
const THP2_2_ADDRESSES_SQLITE_DDL: &str = r#"
CREATE TABLE addresses (
    id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
    display_name TEXT,
    latitude_e6 INTEGER,
    latitude_e6_is_nan INTEGER NOT NULL CHECK(latitude_e6_is_nan IN (0, 1)),
    longitude_e6 INTEGER,
    longitude_e6_is_nan INTEGER NOT NULL CHECK(longitude_e6_is_nan IN (0, 1)),
    name TEXT,
    house_number TEXT,
    road TEXT,
    neighbourhood TEXT,
    city TEXT,
    county TEXT,
    postcode TEXT,
    state TEXT,
    state_district TEXT,
    country TEXT,
    inserted_at_pg_us INTEGER NOT NULL CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0)),
    updated_at_pg_us INTEGER NOT NULL CHECK(updated_at_pg_us = (-9223372036854775807 - 1) OR updated_at_pg_us = 9223372036854775807 OR (updated_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND updated_at_pg_us % 1000000 = 0)),
    osm_id INTEGER,
    osm_type TEXT,
    CHECK((latitude_e6 IS NULL AND latitude_e6_is_nan IN (0, 1)) OR (latitude_e6 IS NOT NULL AND latitude_e6_is_nan = 0 AND latitude_e6 BETWEEN -99999999 AND 99999999)),
    CHECK((longitude_e6 IS NULL AND longitude_e6_is_nan IN (0, 1)) OR (longitude_e6 IS NOT NULL AND longitude_e6_is_nan = 0 AND longitude_e6 BETWEEN -999999999 AND 999999999))
) STRICT, WITHOUT ROWID
"#;
const THP2_2_GEOFENCE_SLICE_SHA256: &str =
    "c64f07b35c76d248d3538a741d26d1d4293ee97cc01e5097f2e86f08f82e0981";
#[cfg(test)]
const THP2_2_GEOFENCE_SLICE_CONTRACT: &str = concat!(
    "teslatlas-thp2.2-geofences-v1\n",
    "id:integer:not_null\n",
    "name:character varying(255):not_null\n",
    "latitude:numeric(8,6):not_null:finite_scaled_i64_or_nan:e6\n",
    "longitude:numeric(9,6):not_null:finite_scaled_i64_or_nan:e6\n",
    "radius:smallint:not_null\n",
    "billing_type:billing_type:not_null\n",
    "cost_per_unit:numeric(9,4):nullable:finite_scaled_i64_or_nan:e4\n",
    "session_fee:numeric(14,2):nullable:finite_scaled_i64_or_nan:e2\n",
    "inserted_at:timestamp(0) without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000_timestamp_0\n",
    "updated_at:timestamp(0) without time zone:not_null:postgres_timestamp_binary_i64_us_since_2000_timestamp_0\n",
);
const THP2_2_GEOFENCES_SQLITE_DDL: &str = r#"
CREATE TABLE geofences (
    id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
    name TEXT NOT NULL CHECK(length(name) <= 255),
    latitude_e6 INTEGER,
    latitude_e6_is_nan INTEGER NOT NULL CHECK(latitude_e6_is_nan IN (0, 1)),
    longitude_e6 INTEGER,
    longitude_e6_is_nan INTEGER NOT NULL CHECK(longitude_e6_is_nan IN (0, 1)),
    radius INTEGER NOT NULL CHECK(radius BETWEEN -32768 AND 32767),
    billing_type TEXT NOT NULL CHECK(billing_type IN ('per_kwh', 'per_minute')),
    cost_per_unit_e4 INTEGER,
    cost_per_unit_e4_is_nan INTEGER NOT NULL CHECK(cost_per_unit_e4_is_nan IN (0, 1)),
    session_fee_e2 INTEGER,
    session_fee_e2_is_nan INTEGER NOT NULL CHECK(session_fee_e2_is_nan IN (0, 1)),
    inserted_at_pg_us INTEGER NOT NULL CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0)),
    updated_at_pg_us INTEGER NOT NULL CHECK(updated_at_pg_us = (-9223372036854775807 - 1) OR updated_at_pg_us = 9223372036854775807 OR (updated_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND updated_at_pg_us % 1000000 = 0)),
    CHECK((latitude_e6 IS NULL AND latitude_e6_is_nan = 1) OR (latitude_e6 IS NOT NULL AND latitude_e6_is_nan = 0 AND latitude_e6 BETWEEN -99999999 AND 99999999)),
    CHECK((longitude_e6 IS NULL AND longitude_e6_is_nan = 1) OR (longitude_e6 IS NOT NULL AND longitude_e6_is_nan = 0 AND longitude_e6 BETWEEN -999999999 AND 999999999)),
    CHECK((cost_per_unit_e4 IS NULL AND cost_per_unit_e4_is_nan IN (0, 1)) OR (cost_per_unit_e4 IS NOT NULL AND cost_per_unit_e4_is_nan = 0 AND cost_per_unit_e4 BETWEEN -999999 AND 999999)),
    CHECK((session_fee_e2 IS NULL AND session_fee_e2_is_nan IN (0, 1)) OR (session_fee_e2 IS NOT NULL AND session_fee_e2_is_nan = 0 AND session_fee_e2 BETWEEN -999999 AND 999999))
) STRICT, WITHOUT ROWID
"#;

// The original THP1 projection contract consumed by the released client.
// Schema 2.1 is additive; do not borrow any of its fields or widened types
// when emitting a schema-2.0 object.
const HUB_PROJECTION_SCHEMA_V1_SQL: &str = r#"
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
"#;

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
    if chunks
        .first()
        .is_some_and(|built| built.metadata.schema == HUB_PROJECTION_SCHEMA_V3)
    {
        validate_binding_v2_2(binding)?;
    } else {
        validate_binding(binding)?;
    }
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
    if schema != HUB_PROJECTION_SCHEMA_V1
        && schema != HUB_PROJECTION_SCHEMA_V2
        && schema != HUB_PROJECTION_SCHEMA_V3
    {
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

    if total_rows != transport_rows {
        return Err(invalid("manifest row total does not match transport rows"));
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

/// Whether this candidate created the immutable content-addressed file it
/// references.  The bit is a deletion right, not a statement about whether
/// the object is valid: both variants have passed the same verification.
///
/// A caller may remove an unpublished pack only when it holds `Created`.
/// A `ReusedExisting` pack may already be referenced by a committed catalog
/// entry owned by another candidate, so removing it would corrupt that entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionPackOwnership {
    Created,
    ReusedExisting,
}

/// Durable cleanup receipt for the private staging name used to install a
/// verified content object. `PendingStartupRepair` is still a successful pack
/// publication: the final content name and its directory were synced first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionPackCleanupState {
    Complete,
    PendingStartupRepair,
}

/// A complete, verified immutable object ready for the existing pack catalog.
#[derive(Debug)]
pub struct BuiltProjectionPack {
    pub metadata: TransportPack,
    pub path: PathBuf,
    pub verified: VerifiedTransportPack,
    ownership: ProjectionPackOwnership,
    cleanup_state: ProjectionPackCleanupState,
}

impl BuiltProjectionPack {
    /// Return whether this value created the on-disk content-addressed object.
    pub fn ownership(&self) -> ProjectionPackOwnership {
        self.ownership
    }

    pub fn cleanup_state(&self) -> ProjectionPackCleanupState {
        self.cleanup_state
    }

    /// Candidate cleanup has a deletion right only for a newly linked pack.
    /// Keep this crate-visible so all cleanup paths share the same ownership
    /// boundary instead of open-coding a path-based guess.
    pub(crate) fn may_remove_unpublished_file(&self) -> bool {
        self.ownership == ProjectionPackOwnership::Created
    }
}

impl Clone for BuiltProjectionPack {
    fn clone(&self) -> Self {
        // A clone is only another descriptor for the immutable file. It did
        // not create the hard link, so it must never receive the one-time
        // cleanup right held by the original candidate.
        Self {
            metadata: self.metadata.clone(),
            path: self.path.clone(),
            verified: self.verified,
            ownership: ProjectionPackOwnership::ReusedExisting,
            cleanup_state: self.cleanup_state,
        }
    }
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
        self.ensure_full_snapshot_capacity_for_capture(u64::MAX, minimum_free_bytes)
    }

    /// Refuse a source capture unless its validated capture bound, the active
    /// SQLite/compression pair, and the caller's free-space reserve fit on the
    /// target filesystem. The capture bound is doubled for SQLite and parent
    /// row duplication, then clamped by the negotiated protocol ceiling. This
    /// keeps admission tied to the source's bounded import contract instead of
    /// reserving the entire wire-format safety ceiling for every small source.
    pub fn ensure_full_snapshot_capacity_for_capture(
        &self,
        capture_bound_bytes: u64,
        minimum_free_bytes: u64,
    ) -> Result<(), ProjectionPackError> {
        let protocol_final_bytes = u64::try_from(self.limits.max_chunks)
            .map_err(|_| ProjectionPackError::CapacityOverflow)?
            .checked_mul(self.limits.max_compressed_pack_bytes)
            .ok_or(ProjectionPackError::CapacityOverflow)?;
        let capture_final_bytes = capture_bound_bytes
            .checked_mul(2)
            .ok_or(ProjectionPackError::CapacityOverflow)?;
        let final_bytes = protocol_final_bytes.min(capture_final_bytes);
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
        validate_v1_snapshot(request)?;
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

        let mut compressed_temp = StagedFile::create(&staging_dir, "projection.zst")?;
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
        let publication =
            publish_immutable(&mut compressed_temp, &final_path, &metadata, self.limits)?;

        Ok(BuiltProjectionPack {
            metadata,
            path: final_path,
            verified,
            ownership: publication.ownership,
            cleanup_state: publication.cleanup_state,
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
        let mut compressed_temp = StagedFile::create(&staging_dir, "projection.zst")?;
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
        let publication =
            publish_immutable(&mut compressed_temp, &final_path, &metadata, self.limits)?;
        Ok(BuiltProjectionPack {
            metadata,
            path: final_path,
            verified,
            ownership: publication.ownership,
            cleanup_state: publication.cleanup_state,
        })
    }

    /// Write and locally verify one full schema-2.2 snapshot.
    ///
    /// Schema 2.2 is full-snapshot-only. The caller signs the returned object
    /// and catalogues it through `HubStore`.
    pub fn write_full_snapshot_2_2(
        &self,
        request: &ProjectionPackRequestV2_2<'_>,
    ) -> Result<BuiltProjectionPack, ProjectionPackError> {
        let row_count = validate_request_v2_2(request, self.limits)?;
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

        let sqlite_temp = StagedFile::create(&staging_dir, "projection-2-2.sqlite")?;
        write_projection_sqlite_2_2(sqlite_temp.path(), request, self.limits, row_count)?;
        verify_projection_sqlite_2_2(sqlite_temp.path(), request, row_count)?;
        let uncompressed_bytes = fs::metadata(sqlite_temp.path())
            .map_err(|source| ProjectionPackError::Metadata {
                path: sqlite_temp.path().to_path_buf(),
                source,
            })?
            .len();

        let mut compressed_temp = StagedFile::create(&staging_dir, "projection-2-2.zst")?;
        let (sha256, compressed_bytes) = compress_file(sqlite_temp.path(), compressed_temp.path())?;
        let metadata = TransportPack {
            pack_id: request.pack_id,
            snapshot_id: request.snapshot_id,
            ordinal: request.ordinal,
            schema: HUB_PROJECTION_SCHEMA_V3,
            format: PackFormat::HubProjectionSqlite,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(sha256),
            sha256,
            compressed_bytes,
            uncompressed_bytes,
            row_count,
            sequence: request.sequence,
            tables: tables_for_snapshot_v2_2(request.snapshot),
        };
        metadata.validate(self.limits)?;
        let verified = verify_file(&metadata, compressed_temp.path(), self.limits)?;
        let final_path = self.content_path(sha256);
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        let publication =
            publish_immutable(&mut compressed_temp, &final_path, &metadata, self.limits)?;
        Ok(BuiltProjectionPack {
            metadata,
            path: final_path,
            verified,
            ownership: publication.ownership,
            cleanup_state: publication.cleanup_state,
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
        let mut compressed_temp = StagedFile::create(&staging_dir, "projection-delta.zst")?;
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
        let publication =
            publish_immutable(&mut compressed_temp, &final_path, &metadata, self.limits)?;
        Ok(BuiltProjectionPack {
            metadata,
            path: final_path,
            verified,
            ownership: publication.ownership,
            cleanup_state: publication.cleanup_state,
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
    validate_car_settings(&car.settings)?;

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

/// Validate the separate schema-2.2 local physical snapshot. Selected-car
/// scope and the charge-to-extant-process source query boundary are checked in
/// Rust; V3 SQLite deliberately carries no local source FKs or normalized
/// compatibility rows.
fn validate_request_v2_2(
    request: &ProjectionPackRequestV2_2<'_>,
    limits: ProtocolLimits,
) -> Result<u64, ProjectionPackError> {
    if request.pack_id.is_nil() || request.snapshot_id.is_nil() {
        return Err(invalid("pack and snapshot IDs must not be nil"));
    }
    if request.ordinal != 0 {
        return Err(invalid("schema 2.2 full snapshot must use ordinal 0"));
    }
    validate_binding_v2_2(&request.binding)?;
    if !request.sequence.is_ordered() {
        return Err(invalid("full snapshot sequence is unordered"));
    }
    let snapshot = request.snapshot;
    let row_count = snapshot.row_count()?;
    if row_count > limits.max_rows_per_pack {
        return Err(ProjectionPackError::TooManyRows);
    }
    if snapshot.global_settings.len() != 1 {
        return Err(invalid(
            "schema 2.2 physical snapshot must contain exactly one global_settings row",
        ));
    }
    let global_settings = &snapshot.global_settings[0];
    validate_optional_text_with_source_width(
        global_settings.base_url.as_deref(),
        255,
        "global_settings.base_url",
    )?;
    validate_optional_text_with_source_width(
        global_settings.grafana_url.as_deref(),
        255,
        "global_settings.grafana_url",
    )?;
    // Required source TEXT permits the empty string. Keep only the generic
    // safety bound; neither field has a reviewed vocabulary restriction.
    validate_optional_text(Some(&global_settings.language), "global_settings.language")?;
    validate_optional_text(
        Some(&global_settings.theme_mode),
        "global_settings.theme_mode",
    )?;
    validate_timestamp_0_pg_us(
        global_settings.inserted_at_pg_us,
        "global_settings.inserted_at_pg_us",
    )?;
    validate_timestamp_0_pg_us(
        global_settings.updated_at_pg_us,
        "global_settings.updated_at_pg_us",
    )?;
    if snapshot.cars.len() != 1 {
        return Err(invalid(
            "one vehicle projection must contain exactly one car",
        ));
    }
    if snapshot.car_settings.len() != 1 {
        return Err(invalid(
            "one vehicle projection must contain exactly one car_settings row",
        ));
    }

    let car = &snapshot.cars[0];
    if i64::from(car.id) != request.binding.selected_car_id {
        return Err(invalid("selected_car_id does not match car.id"));
    }
    // These are physical source values, not the normalized legacy car
    // projection. `efficiency` is encoded as its exact IEEE-754 bit pattern;
    // do not normalize, reject, or convert its FLOAT8 representation.
    validate_optional_text_with_source_width(car.model.as_deref(), 255, "car.model")?;
    validate_optional_text_with_source_width(
        car.marketing_name.as_deref(),
        255,
        "car.marketing_name",
    )?;
    validate_timestamp_0_pg_us(car.inserted_at_pg_us, "car.inserted_at_pg_us")?;
    validate_timestamp_0_pg_us(car.updated_at_pg_us, "car.updated_at_pg_us")?;
    for (value, field) in [
        (car.vin.as_deref(), "car.vin"),
        (car.name.as_deref(), "car.name"),
        (car.trim_badging.as_deref(), "car.trim_badging"),
        (car.exterior_color.as_deref(), "car.exterior_color"),
        (car.wheel_type.as_deref(), "car.wheel_type"),
        (car.spoiler_type.as_deref(), "car.spoiler_type"),
    ] {
        validate_optional_text(value, field)?;
    }
    let car_settings = &snapshot.car_settings[0];
    if car.settings_id != car_settings.id {
        return Err(invalid(
            "car.settings_id does not match the selected car_settings.id",
        ));
    }

    let mut drive_ids = HashSet::with_capacity(snapshot.drives.len());
    let mut referenced_address_ids = HashSet::new();
    let mut referenced_geofence_ids = HashSet::new();
    for drive in &snapshot.drives {
        require_unique_signed_i32(&mut drive_ids, drive.id, "drive.id")?;
        if i64::from(drive.car_id) != request.binding.selected_car_id {
            return Err(invalid("drive.car_id does not match selected_car_id"));
        }
        validate_postgres_timestamp_us(drive.start_date_pg_us, "drive.start_date_pg_us")?;
        if let Some(end_date_pg_us) = drive.end_date_pg_us {
            validate_postgres_timestamp_us(end_date_pg_us, "drive.end_date_pg_us")?;
        }
        for (value, minimum, maximum, field) in [
            (
                drive.outside_temp_avg_e1,
                -9_999,
                9_999,
                "drive.outside_temp_avg_e1",
            ),
            (
                drive.inside_temp_avg_e1,
                -9_999,
                9_999,
                "drive.inside_temp_avg_e1",
            ),
            (
                drive.start_ideal_range_km_e2,
                -999_999,
                999_999,
                "drive.start_ideal_range_km_e2",
            ),
            (
                drive.end_ideal_range_km_e2,
                -999_999,
                999_999,
                "drive.end_ideal_range_km_e2",
            ),
            (
                drive.start_rated_range_km_e2,
                -999_999,
                999_999,
                "drive.start_rated_range_km_e2",
            ),
            (
                drive.end_rated_range_km_e2,
                -999_999,
                999_999,
                "drive.end_rated_range_km_e2",
            ),
        ] {
            validate_optional_fixed_numeric_v2_2(value, minimum, maximum, field)?;
        }
        for id in [drive.start_address_id, drive.end_address_id]
            .into_iter()
            .flatten()
        {
            referenced_address_ids.insert(i64::from(id));
        }
        for id in [drive.start_geofence_id, drive.end_geofence_id]
            .into_iter()
            .flatten()
        {
            referenced_geofence_ids.insert(i64::from(id));
        }
    }

    let mut address_ids = HashSet::with_capacity(snapshot.addresses.len());
    for address in &snapshot.addresses {
        require_unique_signed_i32(&mut address_ids, address.id, "address.id")?;
        validate_optional_text_with_source_width(
            address.display_name.as_deref(),
            512,
            "address.display_name",
        )?;
        validate_optional_fixed_numeric_v2_2(
            address.latitude_e6,
            -99_999_999,
            99_999_999,
            "address.latitude_e6",
        )?;
        validate_optional_fixed_numeric_v2_2(
            address.longitude_e6,
            -999_999_999,
            999_999_999,
            "address.longitude_e6",
        )?;
        for (value, field) in [
            (address.name.as_deref(), "address.name"),
            (address.house_number.as_deref(), "address.house_number"),
            (address.road.as_deref(), "address.road"),
            (address.neighbourhood.as_deref(), "address.neighbourhood"),
            (address.city.as_deref(), "address.city"),
            (address.county.as_deref(), "address.county"),
            (address.postcode.as_deref(), "address.postcode"),
            (address.state.as_deref(), "address.state"),
            (address.state_district.as_deref(), "address.state_district"),
            (address.country.as_deref(), "address.country"),
        ] {
            validate_optional_text_with_source_width(value, 255, field)?;
        }
        // `osm_id` is a nullable source bigint with no source positivity
        // constraint. `osm_type` is source TEXT, so it uses only the generic
        // bounded-string admission.
        validate_optional_text(address.osm_type.as_deref(), "address.osm_type")?;
        validate_timestamp_0_pg_us(address.inserted_at_pg_us, "address.inserted_at_pg_us")?;
        validate_timestamp_0_pg_us(address.updated_at_pg_us, "address.updated_at_pg_us")?;
    }

    let mut geofence_ids = HashSet::with_capacity(snapshot.geofences.len());
    for geofence in &snapshot.geofences {
        require_unique_signed_i32(&mut geofence_ids, geofence.id, "geofence.id")?;
        validate_required_text_with_source_width(&geofence.name, 255, "geofence.name")?;
        // These bounds are the pinned physical `numeric(p,s)` domains, not
        // geography policy. In particular, `(0, 0)` is a valid source value.
        validate_fixed_numeric_v2_2(
            geofence.latitude_e6,
            -99_999_999,
            99_999_999,
            "geofence.latitude_e6",
        )?;
        validate_fixed_numeric_v2_2(
            geofence.longitude_e6,
            -999_999_999,
            999_999_999,
            "geofence.longitude_e6",
        )?;
        // `radius` is already i16, so every physical smallint value—including
        // zero and signed extremes—is deliberately admissible.
        validate_optional_fixed_numeric_v2_2(
            geofence.cost_per_unit_e4,
            -999_999,
            999_999,
            "geofence.cost_per_unit_e4",
        )?;
        validate_optional_fixed_numeric_v2_2(
            geofence.session_fee_e2,
            -999_999,
            999_999,
            "geofence.session_fee_e2",
        )?;
        validate_timestamp_0_pg_us(geofence.inserted_at_pg_us, "geofence.inserted_at_pg_us")?;
        validate_timestamp_0_pg_us(geofence.updated_at_pg_us, "geofence.updated_at_pg_us")?;
    }

    let mut charging_process_ids = HashSet::with_capacity(snapshot.charging_processes.len());
    for process in &snapshot.charging_processes {
        require_unique_signed_i32(&mut charging_process_ids, process.id, "charging_process.id")?;
        if i64::from(process.car_id) != request.binding.selected_car_id {
            return Err(invalid(
                "charging_process.car_id does not match selected_car_id",
            ));
        }
        validate_postgres_timestamp_us(
            process.start_date_pg_us,
            "charging_process.start_date_pg_us",
        )?;
        if let Some(end_date_pg_us) = process.end_date_pg_us {
            validate_postgres_timestamp_us(end_date_pg_us, "charging_process.end_date_pg_us")?;
        }
        for (value, minimum, maximum, field) in [
            (
                process.charge_energy_added_e2,
                -99_999_999,
                99_999_999,
                "charging_process.charge_energy_added_e2",
            ),
            (
                process.charge_energy_used_e2,
                -99_999_999,
                99_999_999,
                "charging_process.charge_energy_used_e2",
            ),
            (
                process.start_ideal_range_km_e2,
                -999_999,
                999_999,
                "charging_process.start_ideal_range_km_e2",
            ),
            (
                process.end_ideal_range_km_e2,
                -999_999,
                999_999,
                "charging_process.end_ideal_range_km_e2",
            ),
            (
                process.start_rated_range_km_e2,
                -999_999,
                999_999,
                "charging_process.start_rated_range_km_e2",
            ),
            (
                process.end_rated_range_km_e2,
                -999_999,
                999_999,
                "charging_process.end_rated_range_km_e2",
            ),
            (
                process.outside_temp_avg_e1,
                -9_999,
                9_999,
                "charging_process.outside_temp_avg_e1",
            ),
            (
                process.cost_e2,
                -999_999,
                999_999,
                "charging_process.cost_e2",
            ),
        ] {
            validate_optional_fixed_numeric_v2_2(value, minimum, maximum, field)?;
        }
        // Source FKs remain physical values in this V3 local snapshot. In
        // particular, `position_id` can name a valid cross-car target omitted
        // by the selected-car subset, so no SQLite closure is invented.
        if let Some(address_id) = process.address_id {
            referenced_address_ids.insert(i64::from(address_id));
        }
        if let Some(geofence_id) = process.geofence_id {
            referenced_geofence_ids.insert(i64::from(geofence_id));
        }
    }

    // Source optional address/geofence references stay soft in this local
    // subset. Their source targets can be extant but omitted here, and source
    // constraint state is not re-attested by V3 SQLite. Any loaded physical
    // address/geofence row must still be selected-car referenced.
    if let Some(unreferenced) = address_ids
        .iter()
        .find(|id| !referenced_address_ids.contains(id))
    {
        return Err(invalid(format!(
            "address {unreferenced} is not referenced by the selected car"
        )));
    }
    if let Some(unreferenced) = geofence_ids
        .iter()
        .find(|id| !referenced_geofence_ids.contains(id))
    {
        return Err(invalid(format!(
            "geofence {unreferenced} is not referenced by the selected car"
        )));
    }

    let mut position_ids = HashSet::with_capacity(snapshot.positions.len());
    for position in &snapshot.positions {
        require_unique_signed_i32(&mut position_ids, position.id, "position.id")?;
        if i64::from(position.car_id) != request.binding.selected_car_id {
            return Err(invalid("position.car_id does not match selected_car_id"));
        }
        validate_postgres_timestamp_us(position.date_pg_us, "position.date_pg_us")?;
        // The source `drive_id` FK is intentionally not reproduced as a pack
        // FK: a selected-car physical slice can retain an extant cross-car
        // drive ID while omitting that target. Car scope remains a Rust
        // admission boundary.
        validate_fixed_numeric_v2_2(
            position.latitude_e6,
            -99_999_999,
            99_999_999,
            "position.latitude_e6",
        )?;
        validate_fixed_numeric_v2_2(
            position.longitude_e6,
            -999_999_999,
            999_999_999,
            "position.longitude_e6",
        )?;
        for (value, minimum, maximum, field) in [
            (
                position.ideal_battery_range_km_e2,
                -999_999,
                999_999,
                "position.ideal_battery_range_km_e2",
            ),
            (
                position.est_battery_range_km_e2,
                -999_999,
                999_999,
                "position.est_battery_range_km_e2",
            ),
            (
                position.rated_battery_range_km_e2,
                -999_999,
                999_999,
                "position.rated_battery_range_km_e2",
            ),
            (
                position.outside_temp_e1,
                -9_999,
                9_999,
                "position.outside_temp_e1",
            ),
            (
                position.inside_temp_e1,
                -9_999,
                9_999,
                "position.inside_temp_e1",
            ),
            (
                position.driver_temp_setting_e1,
                -9_999,
                9_999,
                "position.driver_temp_setting_e1",
            ),
            (
                position.passenger_temp_setting_e1,
                -9_999,
                9_999,
                "position.passenger_temp_setting_e1",
            ),
            (
                position.tpms_pressure_fl_e1,
                -9_999,
                9_999,
                "position.tpms_pressure_fl_e1",
            ),
            (
                position.tpms_pressure_fr_e1,
                -9_999,
                9_999,
                "position.tpms_pressure_fr_e1",
            ),
            (
                position.tpms_pressure_rl_e1,
                -9_999,
                9_999,
                "position.tpms_pressure_rl_e1",
            ),
            (
                position.tpms_pressure_rr_e1,
                -9_999,
                9_999,
                "position.tpms_pressure_rr_e1",
            ),
        ] {
            validate_optional_fixed_numeric_v2_2(value, minimum, maximum, field)?;
        }
    }

    let mut charge_ids = HashSet::with_capacity(snapshot.charges.len());
    for charge in &snapshot.charges {
        require_unique_signed_i32(&mut charge_ids, charge.id, "charge.id")?;
        if !charging_process_ids.contains(&i64::from(charge.charging_process_id)) {
            return Err(invalid(
                "charge.charging_process_id is not present in this local physical slice",
            ));
        }
        validate_postgres_timestamp_us(charge.date_pg_us, "charge.date_pg_us")?;
        validate_fixed_numeric_v2_2(
            charge.charge_energy_added_e2,
            -99_999_999,
            99_999_999,
            "charge.charge_energy_added_e2",
        )?;
        validate_fixed_numeric_v2_2(
            charge.ideal_battery_range_km_e2,
            -999_999,
            999_999,
            "charge.ideal_battery_range_km_e2",
        )?;
        validate_optional_fixed_numeric_v2_2(
            charge.rated_battery_range_km_e2,
            -999_999,
            999_999,
            "charge.rated_battery_range_km_e2",
        )?;
        validate_optional_fixed_numeric_v2_2(
            charge.outside_temp_e1,
            -9_999,
            9_999,
            "charge.outside_temp_e1",
        )?;
        for (value, field) in [
            (
                charge.conn_charge_cable.as_deref(),
                "charge.conn_charge_cable",
            ),
            (
                charge.fast_charger_brand.as_deref(),
                "charge.fast_charger_brand",
            ),
            (
                charge.fast_charger_type.as_deref(),
                "charge.fast_charger_type",
            ),
        ] {
            validate_optional_text_with_source_width(value, 255, field)?;
        }
    }
    validate_states_v2_2(&snapshot.states, request.binding.selected_car_id)?;
    validate_updates_v2_2(&snapshot.updates, request.binding.selected_car_id)?;
    Ok(row_count)
}

/// Schema 2.0 predates standalone position history and stores `power` as an
/// INTEGER. Keep this narrowing explicit so a pack is never labelled 2.0 and
/// then rejected by the released 2.0 client after transport succeeds.
fn validate_v1_snapshot(request: &ProjectionPackRequest<'_>) -> Result<(), ProjectionPackError> {
    for position in &request.snapshot.positions {
        if position.drive_id.is_none() {
            return Err(invalid("schema 2.0 position.drive_id must be present"));
        }
        let _ = v1_position_power(position.power)?;
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
        validate_car_settings(&car.settings)?;
    }
    let mut setting_ids = HashSet::with_capacity(delta.car_settings.len());
    for patch in &delta.car_settings {
        require_unique_positive(&mut setting_ids, patch.car_id, "car_settings.car_id")?;
        require_same_car(patch.car_id, selected_car_id, "car_settings.car_id")?;
        if car_ids.contains(&patch.car_id) {
            return Err(invalid("car upsert and car settings patch overlap"));
        }
        validate_car_settings(&patch.settings)?;
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
        if charge
            .end_date_ms
            .is_some_and(|end| end < charge.start_date_ms)
        {
            return Err(invalid("charge.end_date_ms precedes start_date_ms"));
        }
        validate_optional_nonnegative(charge.charge_energy_added, "charge.charge_energy_added")?;
        validate_optional_finite(charge.cost, "charge.cost")?;
        validate_coordinate_pair(
            charge.start_latitude,
            charge.start_longitude,
            "charge.start",
        )?;
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
        validate_optional_soc(
            position.usable_battery_level,
            "position.usable_battery_level",
        )?;
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

    let upsert_ids = delta_upsert_ids(delta);
    let mut tombstone_ids = HashSet::with_capacity(delta.tombstones.len());
    for tombstone in &delta.tombstones {
        require_positive(tombstone.id, "tombstone.id")?;
        require_same_car(tombstone.car_id, selected_car_id, "tombstone.car_id")?;
        if tombstone.entity.source_owned_tombstone_order().is_none() {
            return Err(invalid(format!(
                "unsupported source-owned delta tombstone entity {}",
                tombstone.entity.as_str()
            )));
        }
        if !tombstone_ids.insert((tombstone.entity, tombstone.id)) {
            return Err(invalid("duplicate typed tombstone"));
        }
        if upsert_ids.contains(&(tombstone.entity, tombstone.id)) {
            return Err(invalid("typed delta upsert and tombstone overlap"));
        }
    }
    let row_count = delta.row_count()?;
    if row_count == 0 || row_count > limits.max_rows_per_pack {
        return Err(ProjectionPackError::TooManyRows);
    }
    Ok(row_count)
}

fn delta_upsert_ids(delta: &ProjectionDelta) -> HashSet<(ProjectionDeltaEntity, i64)> {
    let mut ids = HashSet::new();
    ids.extend(
        delta
            .cars
            .iter()
            .map(|row| (ProjectionDeltaEntity::Car, row.id)),
    );
    ids.extend(
        delta
            .car_settings
            .iter()
            .map(|row| (ProjectionDeltaEntity::CarSetting, row.car_id)),
    );
    ids.extend(
        delta
            .drives
            .iter()
            .map(|row| (ProjectionDeltaEntity::Drive, row.id)),
    );
    ids.extend(
        delta
            .positions
            .iter()
            .map(|row| (ProjectionDeltaEntity::Position, row.id)),
    );
    ids.extend(
        delta
            .charges
            .iter()
            .map(|row| (ProjectionDeltaEntity::Charge, row.id)),
    );
    ids.extend(
        delta
            .charge_samples
            .iter()
            .map(|row| (ProjectionDeltaEntity::ChargeSample, row.id)),
    );
    ids.extend(
        delta
            .states
            .iter()
            .map(|row| (ProjectionDeltaEntity::State, row.id)),
    );
    ids.extend(
        delta
            .updates
            .iter()
            .map(|row| (ProjectionDeltaEntity::Update, row.id)),
    );
    ids
}

fn source_owned_tombstones_in_canonical_order(
    values: &[ProjectionTombstone],
) -> Vec<&ProjectionTombstone> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| {
        (
            row.entity.source_owned_tombstone_order().unwrap_or(u8::MAX),
            row.id,
        )
    });
    rows
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
        validate_required_text(&update.version, "update.version")?;
    }
    Ok(())
}

/// Validate the raw physical state slice without importing compatibility
/// policies such as positive identifiers, interval ordering, or a single open
/// state. PostgreSQL's signed int4/timestamp domains are represented exactly.
fn validate_states_v2_2(
    states: &[ProjectionStateV2_2],
    selected_car_id: i64,
) -> Result<(), ProjectionPackError> {
    let mut ids = HashSet::with_capacity(states.len());
    for state in states {
        if !ids.insert(state.id) {
            return Err(invalid("state.id is duplicated"));
        }
        if i64::from(state.car_id) != selected_car_id {
            return Err(invalid("state.car_id does not match selected car"));
        }
        validate_postgres_timestamp_us(state.start_date_pg_us, "state.start_date_pg_us")?;
        if let Some(end_date_pg_us) = state.end_date_pg_us {
            validate_postgres_timestamp_us(end_date_pg_us, "state.end_date_pg_us")?;
        }
    }
    Ok(())
}

/// Validate the raw physical update slice without applying the legacy
/// completed-update, interval, trimming, or defaulting rules.
fn validate_updates_v2_2(
    updates: &[ProjectionUpdateV2_2],
    selected_car_id: i64,
) -> Result<(), ProjectionPackError> {
    let mut ids = HashSet::with_capacity(updates.len());
    for update in updates {
        if !ids.insert(update.id) {
            return Err(invalid("update.id is duplicated"));
        }
        if i64::from(update.car_id) != selected_car_id {
            return Err(invalid("update.car_id does not match selected car"));
        }
        validate_postgres_timestamp_us(update.start_date_pg_us, "update.start_date_pg_us")?;
        if let Some(end_date_pg_us) = update.end_date_pg_us {
            validate_postgres_timestamp_us(end_date_pg_us, "update.end_date_pg_us")?;
        }
        validate_optional_text_with_source_width(update.version.as_deref(), 255, "update.version")?;
    }
    Ok(())
}

fn validate_postgres_timestamp_us(value: i64, field: &str) -> Result<(), ProjectionPackError> {
    if value == i64::MIN
        || value == i64::MAX
        || (POSTGRES_TIMESTAMP_FINITE_MIN_US..POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US)
            .contains(&value)
    {
        return Ok(());
    }
    Err(invalid(format!(
        "{field} is outside the PostgreSQL timestamp source domain"
    )))
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

/// Schema 2.2 carries the physical TeslaMate `cars.id` domain: source
/// `smallint` permits signed and zero values even though legacy projection
/// bindings deliberately require a positive normalized mirror identity.
fn validate_binding_v2_2(binding: &ProjectionBinding) -> Result<(), ProjectionPackError> {
    if binding.installation_id.is_nil()
        || binding.account_id.is_nil()
        || binding.vehicle_id.is_nil()
        || binding.generation == 0
    {
        return Err(invalid("projection binding is incomplete"));
    }
    if i16::try_from(binding.selected_car_id).is_err() {
        return Err(invalid(
            "schema 2.2 selected_car_id is outside the TeslaMate smallint source domain",
        ));
    }
    Ok(())
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

fn tables_for_snapshot_v2_2(snapshot: &ProjectionSnapshotV2_2) -> Vec<MirrorTable> {
    // The protocol's current table vocabulary intentionally has no
    // address/geofence variants.  Schema 2.2 is locally validated and cannot
    // reach the catalogue, so retain only the established logical streams in
    // `TransportPack` metadata.  The SQLite verifier below checks the full
    // exact local physical layout, including optional address/geofence rows
    // and raw drive references that intentionally remain soft.
    let mut tables = vec![MirrorTable::Car];
    if !snapshot.drives.is_empty() {
        tables.push(MirrorTable::Drive);
    }
    // Protocol `Charge` is the parent/session vocabulary. The source-shaped
    // physical table is `charging_processes`, so advertise it as the parent.
    if !snapshot.charging_processes.is_empty() {
        tables.push(MirrorTable::Charge);
    }
    if !snapshot.positions.is_empty() {
        tables.push(MirrorTable::Position);
    }
    // Protocol `ChargeSample` is the child vocabulary. Exact physical source
    // `charges` rows retain that child stream without compatibility reshape.
    if !snapshot.charges.is_empty() {
        tables.push(MirrorTable::ChargeSample);
    }
    if !snapshot.states.is_empty() {
        tables.push(MirrorTable::State);
    }
    if !snapshot.updates.is_empty() {
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
    if schema == HUB_PROJECTION_SCHEMA_V1 {
        connection
            .execute_batch(HUB_PROJECTION_SCHEMA_V1_SQL)
            .map_err(ProjectionPackError::CreateSchema)?;
    } else if schema != HUB_PROJECTION_SCHEMA_V2 {
        return Err(invalid("unsupported projection schema"));
    } else {
        // `car_settings` is part of the additive 2.1 layout. Keep its DDL
        // byte-for-byte stable for 2.1 because the typed-delta verifier pins
        // that physical contract separately.
        let car_settings_schema = r#"
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
            ) STRICT, WITHOUT ROWID;"#;
        let schema_sql = format!(
            r#"
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
            ) STRICT, WITHOUT ROWID;{car_settings_schema}
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
            "#
        );
        connection
            .execute_batch(&schema_sql)
            .map_err(ProjectionPackError::CreateSchema)?;
    }
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
    if schema == HUB_PROJECTION_SCHEMA_V1 {
        insert_legacy_cars(&transaction, &request.snapshot.cars)?;
        insert_legacy_drives(&transaction, &request.snapshot.drives)?;
        insert_legacy_charges(&transaction, &request.snapshot.charges)?;
        insert_legacy_positions(&transaction, &request.snapshot.positions)?;
    } else {
        insert_cars(&transaction, &request.snapshot.cars, true)?;
        insert_drives(&transaction, &request.snapshot.drives)?;
        insert_charges(&transaction, &request.snapshot.charges)?;
        insert_positions(&transaction, &request.snapshot.positions)?;
    }
    insert_charge_samples(&transaction, &request.snapshot.charge_samples)?;
    if schema == HUB_PROJECTION_SCHEMA_V2 {
        insert_states(&transaction, states)?;
        insert_updates(&transaction, updates)?;
    }
    crate::durability_fault::check(crate::durability_fault::DurabilityFaultPoint::PackSqliteCommit)
        .map_err(ProjectionPackError::Durability)?;
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

fn write_projection_sqlite_2_2(
    path: &Path,
    request: &ProjectionPackRequestV2_2<'_>,
    limits: ProtocolLimits,
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
            r#"
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
            CREATE TABLE car_settings (
                id INTEGER PRIMARY KEY,
                suspend_min INTEGER NOT NULL CHECK(suspend_min BETWEEN -2147483648 AND 2147483647),
                suspend_after_idle_min INTEGER NOT NULL CHECK(suspend_after_idle_min BETWEEN -2147483648 AND 2147483647),
                req_not_unlocked INTEGER NOT NULL CHECK(req_not_unlocked IN (0, 1)),
                free_supercharging INTEGER NOT NULL CHECK(free_supercharging IN (0, 1)),
                use_streaming_api INTEGER NOT NULL CHECK(use_streaming_api IN (0, 1)),
                enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
                lfp_battery INTEGER NOT NULL CHECK(lfp_battery IN (0, 1))
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE cars (
                id INTEGER PRIMARY KEY CHECK(id BETWEEN -32768 AND 32767),
                eid INTEGER NOT NULL,
                vid INTEGER NOT NULL,
                vin TEXT,
                name TEXT,
                model TEXT CHECK(model IS NULL OR length(model) <= 255),
                efficiency BLOB CHECK(efficiency IS NULL OR length(efficiency) = 8),
                trim_badging TEXT,
                marketing_name TEXT CHECK(marketing_name IS NULL OR length(marketing_name) <= 255),
                exterior_color TEXT,
                wheel_type TEXT,
                spoiler_type TEXT,
                display_priority INTEGER NOT NULL CHECK(display_priority BETWEEN -32768 AND 32767),
                inserted_at_pg_us INTEGER NOT NULL CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0)),
                updated_at_pg_us INTEGER NOT NULL CHECK(updated_at_pg_us = (-9223372036854775807 - 1) OR updated_at_pg_us = 9223372036854775807 OR (updated_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND updated_at_pg_us % 1000000 = 0)),
                settings_id INTEGER NOT NULL UNIQUE REFERENCES car_settings(id)
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE addresses (
                id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
                display_name TEXT,
                latitude_e6 INTEGER,
                latitude_e6_is_nan INTEGER NOT NULL CHECK(latitude_e6_is_nan IN (0, 1)),
                longitude_e6 INTEGER,
                longitude_e6_is_nan INTEGER NOT NULL CHECK(longitude_e6_is_nan IN (0, 1)),
                name TEXT,
                house_number TEXT,
                road TEXT,
                neighbourhood TEXT,
                city TEXT,
                county TEXT,
                postcode TEXT,
                state TEXT,
                state_district TEXT,
                country TEXT,
                inserted_at_pg_us INTEGER NOT NULL CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0)),
                updated_at_pg_us INTEGER NOT NULL CHECK(updated_at_pg_us = (-9223372036854775807 - 1) OR updated_at_pg_us = 9223372036854775807 OR (updated_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND updated_at_pg_us % 1000000 = 0)),
                osm_id INTEGER,
                osm_type TEXT,
                CHECK((latitude_e6 IS NULL AND latitude_e6_is_nan IN (0, 1)) OR (latitude_e6 IS NOT NULL AND latitude_e6_is_nan = 0 AND latitude_e6 BETWEEN -99999999 AND 99999999)),
                CHECK((longitude_e6 IS NULL AND longitude_e6_is_nan IN (0, 1)) OR (longitude_e6 IS NOT NULL AND longitude_e6_is_nan = 0 AND longitude_e6 BETWEEN -999999999 AND 999999999))
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE geofences (
                id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
                name TEXT NOT NULL CHECK(length(name) <= 255),
                latitude_e6 INTEGER,
                latitude_e6_is_nan INTEGER NOT NULL CHECK(latitude_e6_is_nan IN (0, 1)),
                longitude_e6 INTEGER,
                longitude_e6_is_nan INTEGER NOT NULL CHECK(longitude_e6_is_nan IN (0, 1)),
                radius INTEGER NOT NULL CHECK(radius BETWEEN -32768 AND 32767),
                billing_type TEXT NOT NULL CHECK(billing_type IN ('per_kwh', 'per_minute')),
                cost_per_unit_e4 INTEGER,
                cost_per_unit_e4_is_nan INTEGER NOT NULL CHECK(cost_per_unit_e4_is_nan IN (0, 1)),
                session_fee_e2 INTEGER,
                session_fee_e2_is_nan INTEGER NOT NULL CHECK(session_fee_e2_is_nan IN (0, 1)),
                inserted_at_pg_us INTEGER NOT NULL CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0)),
                updated_at_pg_us INTEGER NOT NULL CHECK(updated_at_pg_us = (-9223372036854775807 - 1) OR updated_at_pg_us = 9223372036854775807 OR (updated_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND updated_at_pg_us % 1000000 = 0)),
                CHECK((latitude_e6 IS NULL AND latitude_e6_is_nan = 1) OR (latitude_e6 IS NOT NULL AND latitude_e6_is_nan = 0 AND latitude_e6 BETWEEN -99999999 AND 99999999)),
                CHECK((longitude_e6 IS NULL AND longitude_e6_is_nan = 1) OR (longitude_e6 IS NOT NULL AND longitude_e6_is_nan = 0 AND longitude_e6 BETWEEN -999999999 AND 999999999)),
                CHECK((cost_per_unit_e4 IS NULL AND cost_per_unit_e4_is_nan IN (0, 1)) OR (cost_per_unit_e4 IS NOT NULL AND cost_per_unit_e4_is_nan = 0 AND cost_per_unit_e4 BETWEEN -999999 AND 999999)),
                CHECK((session_fee_e2 IS NULL AND session_fee_e2_is_nan IN (0, 1)) OR (session_fee_e2 IS NOT NULL AND session_fee_e2_is_nan = 0 AND session_fee_e2 BETWEEN -999999 AND 999999))
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE states (
                id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
                car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767) REFERENCES cars(id),
                state TEXT NOT NULL CHECK(state IN ('online', 'offline', 'asleep')),
                start_date_pg_us INTEGER NOT NULL CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
                end_date_pg_us INTEGER CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999)
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE updates (
                id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
                car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767) REFERENCES cars(id),
                start_date_pg_us INTEGER NOT NULL CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
                end_date_pg_us INTEGER CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
                version TEXT CHECK(version IS NULL OR length(version) <= 255)
            ) STRICT, WITHOUT ROWID;
            COMMIT;
            "#,
        )
        .map_err(ProjectionPackError::CreateSchema)?;
    connection
        .execute_batch(THP2_2_DRIVES_SQLITE_DDL)
        .map_err(ProjectionPackError::CreateSchema)?;
    connection
        .execute_batch(THP2_2_GLOBAL_SETTINGS_SQLITE_DDL)
        .map_err(ProjectionPackError::CreateSchema)?;
    connection
        .execute_batch(THP2_2_POSITIONS_SQLITE_DDL)
        .map_err(ProjectionPackError::CreateSchema)?;
    connection
        .execute_batch(THP2_2_CHARGING_PROCESSES_SQLITE_DDL)
        .map_err(ProjectionPackError::CreateSchema)?;
    connection
        .execute_batch(THP2_2_CHARGES_SQLITE_DDL)
        .map_err(ProjectionPackError::CreateSchema)?;

    let transaction = connection
        .unchecked_transaction()
        .map_err(ProjectionPackError::BeginTransaction)?;
    insert_metadata_v2_2(&transaction, request, row_count)?;
    insert_global_settings_v2_2(&transaction, &request.snapshot.global_settings)?;
    insert_car_settings_v2_2(&transaction, &request.snapshot.car_settings)?;
    insert_cars_v2_2(&transaction, &request.snapshot.cars)?;
    insert_addresses_v2_2(&transaction, &request.snapshot.addresses)?;
    insert_geofences_v2_2(&transaction, &request.snapshot.geofences)?;
    insert_drives_v2_2(&transaction, &request.snapshot.drives)?;
    insert_positions_v2_2(&transaction, &request.snapshot.positions)?;
    insert_charging_processes_v2_2(&transaction, &request.snapshot.charging_processes)?;
    insert_charges_v2_2(&transaction, &request.snapshot.charges)?;
    insert_states_v2_2(&transaction, &request.snapshot.states)?;
    insert_updates_v2_2(&transaction, &request.snapshot.updates)?;
    crate::durability_fault::check(crate::durability_fault::DurabilityFaultPoint::PackSqliteCommit)
        .map_err(ProjectionPackError::Durability)?;
    transaction.commit().map_err(ProjectionPackError::Commit)?;
    ensure_foreign_keys_clean(&connection)?;
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
            HUB_PROJECTION_SCHEMA_V3.sqlite_user_version(),
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

fn verify_projection_sqlite_2_2(
    path: &Path,
    request: &ProjectionPackRequestV2_2<'_>,
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(ProjectionPackError::OpenSqlite)?;
    let application_id: u32 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if application_id != SQLITE_HUB_PROJECTION_APPLICATION_ID {
        return Err(invalid("schema 2.2 SQLite application_id is invalid"));
    }
    let user_version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if user_version != HUB_PROJECTION_SCHEMA_V3.sqlite_user_version() {
        return Err(invalid("schema 2.2 SQLite user_version is invalid"));
    }
    let mut tables = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let table_names = tables
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(ProjectionPackError::IntegrityCheck)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let expected_tables = vec![
        "addresses",
        "car_settings",
        "cars",
        "charges",
        "charging_processes",
        "drives",
        "geofences",
        "global_settings",
        "hub_pack_metadata",
        "positions",
        "states",
        "updates",
    ];
    if table_names != expected_tables {
        return Err(invalid("schema 2.2 SQLite table layout is invalid"));
    }
    for (table, without_rowid, expected_columns) in [
        (
            "hub_pack_metadata",
            false,
            &[("key", "TEXT", true, true), ("value", "TEXT", true, false)][..],
        ),
        (
            "cars",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("eid", "INTEGER", true, false),
                ("vid", "INTEGER", true, false),
                ("vin", "TEXT", false, false),
                ("name", "TEXT", false, false),
                ("model", "TEXT", false, false),
                ("efficiency", "BLOB", false, false),
                ("trim_badging", "TEXT", false, false),
                ("marketing_name", "TEXT", false, false),
                ("exterior_color", "TEXT", false, false),
                ("wheel_type", "TEXT", false, false),
                ("spoiler_type", "TEXT", false, false),
                ("display_priority", "INTEGER", true, false),
                ("inserted_at_pg_us", "INTEGER", true, false),
                ("updated_at_pg_us", "INTEGER", true, false),
                ("settings_id", "INTEGER", true, false),
            ][..],
        ),
        (
            "car_settings",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("suspend_min", "INTEGER", true, false),
                ("suspend_after_idle_min", "INTEGER", true, false),
                ("req_not_unlocked", "INTEGER", true, false),
                ("free_supercharging", "INTEGER", true, false),
                ("use_streaming_api", "INTEGER", true, false),
                ("enabled", "INTEGER", true, false),
                ("lfp_battery", "INTEGER", true, false),
            ][..],
        ),
        (
            "addresses",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("display_name", "TEXT", false, false),
                ("latitude_e6", "INTEGER", false, false),
                ("latitude_e6_is_nan", "INTEGER", true, false),
                ("longitude_e6", "INTEGER", false, false),
                ("longitude_e6_is_nan", "INTEGER", true, false),
                ("name", "TEXT", false, false),
                ("house_number", "TEXT", false, false),
                ("road", "TEXT", false, false),
                ("neighbourhood", "TEXT", false, false),
                ("city", "TEXT", false, false),
                ("county", "TEXT", false, false),
                ("postcode", "TEXT", false, false),
                ("state", "TEXT", false, false),
                ("state_district", "TEXT", false, false),
                ("country", "TEXT", false, false),
                ("inserted_at_pg_us", "INTEGER", true, false),
                ("updated_at_pg_us", "INTEGER", true, false),
                ("osm_id", "INTEGER", false, false),
                ("osm_type", "TEXT", false, false),
            ][..],
        ),
        (
            "geofences",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("name", "TEXT", true, false),
                ("latitude_e6", "INTEGER", false, false),
                ("latitude_e6_is_nan", "INTEGER", true, false),
                ("longitude_e6", "INTEGER", false, false),
                ("longitude_e6_is_nan", "INTEGER", true, false),
                ("radius", "INTEGER", true, false),
                ("billing_type", "TEXT", true, false),
                ("cost_per_unit_e4", "INTEGER", false, false),
                ("cost_per_unit_e4_is_nan", "INTEGER", true, false),
                ("session_fee_e2", "INTEGER", false, false),
                ("session_fee_e2_is_nan", "INTEGER", true, false),
                ("inserted_at_pg_us", "INTEGER", true, false),
                ("updated_at_pg_us", "INTEGER", true, false),
            ][..],
        ),
        (
            "global_settings",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("unit_of_length", "TEXT", true, false),
                ("unit_of_temperature", "TEXT", true, false),
                ("unit_of_pressure", "TEXT", true, false),
                ("preferred_range", "TEXT", true, false),
                ("base_url", "TEXT", false, false),
                ("grafana_url", "TEXT", false, false),
                ("language", "TEXT", true, false),
                ("theme_mode", "TEXT", true, false),
                ("inserted_at_pg_us", "INTEGER", true, false),
                ("updated_at_pg_us", "INTEGER", true, false),
            ][..],
        ),
        (
            "drives",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("start_date_pg_us", "INTEGER", true, false),
                ("end_date_pg_us", "INTEGER", false, false),
                ("start_position_id", "INTEGER", false, false),
                ("end_position_id", "INTEGER", false, false),
                ("start_address_id", "INTEGER", false, false),
                ("end_address_id", "INTEGER", false, false),
                ("start_geofence_id", "INTEGER", false, false),
                ("end_geofence_id", "INTEGER", false, false),
                ("outside_temp_avg_e1", "INTEGER", false, false),
                ("outside_temp_avg_e1_is_nan", "INTEGER", true, false),
                ("inside_temp_avg_e1", "INTEGER", false, false),
                ("inside_temp_avg_e1_is_nan", "INTEGER", true, false),
                ("speed_max", "INTEGER", false, false),
                ("power_max", "INTEGER", false, false),
                ("power_min", "INTEGER", false, false),
                ("start_ideal_range_km_e2", "INTEGER", false, false),
                ("start_ideal_range_km_e2_is_nan", "INTEGER", true, false),
                ("end_ideal_range_km_e2", "INTEGER", false, false),
                ("end_ideal_range_km_e2_is_nan", "INTEGER", true, false),
                ("start_rated_range_km_e2", "INTEGER", false, false),
                ("start_rated_range_km_e2_is_nan", "INTEGER", true, false),
                ("end_rated_range_km_e2", "INTEGER", false, false),
                ("end_rated_range_km_e2_is_nan", "INTEGER", true, false),
                ("start_km_f64_be", "BLOB", false, false),
                ("end_km_f64_be", "BLOB", false, false),
                ("distance_f64_be", "BLOB", false, false),
                ("duration_min", "INTEGER", false, false),
                ("ascent", "INTEGER", false, false),
                ("descent", "INTEGER", false, false),
            ][..],
        ),
        (
            "charging_processes",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("position_id", "INTEGER", true, false),
                ("address_id", "INTEGER", false, false),
                ("geofence_id", "INTEGER", false, false),
                ("start_date_pg_us", "INTEGER", true, false),
                ("end_date_pg_us", "INTEGER", false, false),
                ("charge_energy_added_e2", "INTEGER", false, false),
                ("charge_energy_added_e2_is_nan", "INTEGER", true, false),
                ("charge_energy_used_e2", "INTEGER", false, false),
                ("charge_energy_used_e2_is_nan", "INTEGER", true, false),
                ("start_ideal_range_km_e2", "INTEGER", false, false),
                ("start_ideal_range_km_e2_is_nan", "INTEGER", true, false),
                ("end_ideal_range_km_e2", "INTEGER", false, false),
                ("end_ideal_range_km_e2_is_nan", "INTEGER", true, false),
                ("start_rated_range_km_e2", "INTEGER", false, false),
                ("start_rated_range_km_e2_is_nan", "INTEGER", true, false),
                ("end_rated_range_km_e2", "INTEGER", false, false),
                ("end_rated_range_km_e2_is_nan", "INTEGER", true, false),
                ("start_battery_level", "INTEGER", false, false),
                ("end_battery_level", "INTEGER", false, false),
                ("duration_min", "INTEGER", false, false),
                ("outside_temp_avg_e1", "INTEGER", false, false),
                ("outside_temp_avg_e1_is_nan", "INTEGER", true, false),
                ("cost_e2", "INTEGER", false, false),
                ("cost_e2_is_nan", "INTEGER", true, false),
            ][..],
        ),
        (
            "charges",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("charging_process_id", "INTEGER", true, false),
                ("date_pg_us", "INTEGER", true, false),
                ("battery_heater", "INTEGER", false, false),
                ("battery_heater_on", "INTEGER", false, false),
                ("battery_heater_no_power", "INTEGER", false, false),
                ("battery_level", "INTEGER", false, false),
                ("usable_battery_level", "INTEGER", false, false),
                ("charge_energy_added_e2", "INTEGER", false, false),
                ("charge_energy_added_e2_is_nan", "INTEGER", true, false),
                ("charger_actual_current", "INTEGER", false, false),
                ("charger_phases", "INTEGER", false, false),
                ("charger_pilot_current", "INTEGER", false, false),
                ("charger_power", "INTEGER", true, false),
                ("charger_voltage", "INTEGER", false, false),
                ("conn_charge_cable", "TEXT", false, false),
                ("fast_charger_present", "INTEGER", false, false),
                ("fast_charger_brand", "TEXT", false, false),
                ("fast_charger_type", "TEXT", false, false),
                ("ideal_battery_range_km_e2", "INTEGER", false, false),
                ("ideal_battery_range_km_e2_is_nan", "INTEGER", true, false),
                ("rated_battery_range_km_e2", "INTEGER", false, false),
                ("rated_battery_range_km_e2_is_nan", "INTEGER", true, false),
                ("not_enough_power_to_heat", "INTEGER", false, false),
                ("outside_temp_e1", "INTEGER", false, false),
                ("outside_temp_e1_is_nan", "INTEGER", true, false),
            ][..],
        ),
        (
            "positions",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("drive_id", "INTEGER", false, false),
                ("date_pg_us", "INTEGER", true, false),
                ("latitude_e6", "INTEGER", false, false),
                ("latitude_e6_is_nan", "INTEGER", true, false),
                ("longitude_e6", "INTEGER", false, false),
                ("longitude_e6_is_nan", "INTEGER", true, false),
                ("elevation", "INTEGER", false, false),
                ("speed", "INTEGER", false, false),
                ("power", "INTEGER", false, false),
                ("odometer_f64_be", "BLOB", false, false),
                ("ideal_battery_range_km_e2", "INTEGER", false, false),
                ("ideal_battery_range_km_e2_is_nan", "INTEGER", true, false),
                ("est_battery_range_km_e2", "INTEGER", false, false),
                ("est_battery_range_km_e2_is_nan", "INTEGER", true, false),
                ("rated_battery_range_km_e2", "INTEGER", false, false),
                ("rated_battery_range_km_e2_is_nan", "INTEGER", true, false),
                ("battery_level", "INTEGER", false, false),
                ("usable_battery_level", "INTEGER", false, false),
                ("battery_heater", "INTEGER", false, false),
                ("battery_heater_on", "INTEGER", false, false),
                ("battery_heater_no_power", "INTEGER", false, false),
                ("outside_temp_e1", "INTEGER", false, false),
                ("outside_temp_e1_is_nan", "INTEGER", true, false),
                ("inside_temp_e1", "INTEGER", false, false),
                ("inside_temp_e1_is_nan", "INTEGER", true, false),
                ("fan_status", "INTEGER", false, false),
                ("driver_temp_setting_e1", "INTEGER", false, false),
                ("driver_temp_setting_e1_is_nan", "INTEGER", true, false),
                ("passenger_temp_setting_e1", "INTEGER", false, false),
                ("passenger_temp_setting_e1_is_nan", "INTEGER", true, false),
                ("is_climate_on", "INTEGER", false, false),
                ("is_rear_defroster_on", "INTEGER", false, false),
                ("is_front_defroster_on", "INTEGER", false, false),
                ("tpms_pressure_fl_e1", "INTEGER", false, false),
                ("tpms_pressure_fl_e1_is_nan", "INTEGER", true, false),
                ("tpms_pressure_fr_e1", "INTEGER", false, false),
                ("tpms_pressure_fr_e1_is_nan", "INTEGER", true, false),
                ("tpms_pressure_rl_e1", "INTEGER", false, false),
                ("tpms_pressure_rl_e1_is_nan", "INTEGER", true, false),
                ("tpms_pressure_rr_e1", "INTEGER", false, false),
                ("tpms_pressure_rr_e1_is_nan", "INTEGER", true, false),
            ][..],
        ),
        (
            "states",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("state", "TEXT", true, false),
                ("start_date_pg_us", "INTEGER", true, false),
                ("end_date_pg_us", "INTEGER", false, false),
            ][..],
        ),
        (
            "updates",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("start_date_pg_us", "INTEGER", true, false),
                ("end_date_pg_us", "INTEGER", false, false),
                ("version", "TEXT", false, false),
            ][..],
        ),
    ] {
        verify_projection_table_layout(&connection, table, without_rowid, expected_columns)?;
    }
    verify_projection_table_ddl(&connection, "car_settings", THP2_2_CAR_SETTINGS_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "cars", THP2_2_CARS_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "addresses", THP2_2_ADDRESSES_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "geofences", THP2_2_GEOFENCES_SQLITE_DDL)?;
    verify_projection_table_ddl(
        &connection,
        "global_settings",
        THP2_2_GLOBAL_SETTINGS_SQLITE_DDL,
    )?;
    verify_projection_table_ddl(&connection, "drives", THP2_2_DRIVES_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "positions", THP2_2_POSITIONS_SQLITE_DDL)?;
    verify_projection_table_ddl(
        &connection,
        "charging_processes",
        THP2_2_CHARGING_PROCESSES_SQLITE_DDL,
    )?;
    verify_projection_table_ddl(&connection, "charges", THP2_2_CHARGES_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "states", THP2_2_STATES_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "updates", THP2_2_UPDATES_SQLITE_DDL)?;
    for (table, expected_foreign_keys) in [
        ("hub_pack_metadata", &[][..]),
        ("cars", &[("car_settings", "settings_id", "id")][..]),
        ("car_settings", &[][..]),
        ("addresses", &[][..]),
        ("geofences", &[][..]),
        ("global_settings", &[][..]),
        ("drives", &[][..]),
        ("positions", &[][..]),
        ("charging_processes", &[][..]),
        ("charges", &[][..]),
        ("states", &[("cars", "car_id", "id")][..]),
        ("updates", &[("cars", "car_id", "id")][..]),
    ] {
        verify_projection_foreign_keys(&connection, table, expected_foreign_keys)?;
    }
    let expected_metadata = [
        ("protocol", "teslatlas-sync".to_owned()),
        ("pack_format", "hub_projection_sqlite".to_owned()),
        ("schema_major", HUB_PROJECTION_SCHEMA_V3.major.to_string()),
        ("schema_minor", HUB_PROJECTION_SCHEMA_V3.minor.to_string()),
        ("pack_id", request.pack_id.to_string()),
        ("snapshot_id", request.snapshot_id.to_string()),
        ("ordinal", request.ordinal.to_string()),
        ("mode", "full_snapshot".to_owned()),
        ("schema_support", "full_snapshot_only".to_owned()),
        ("publication_scope", "local_validation_only".to_owned()),
        ("ledger_state", "draft_blocked".to_owned()),
        (
            "ledger_slice",
            "settings+car_settings+cars+drives+positions+charging_processes+charges+addresses+geofences+states+updates".to_owned(),
        ),
        ("mapped_fields", THP2_2_MAPPED_FIELD_COUNT.to_string()),
        (
            "unreconciled_fields",
            THP2_2_UNRECONCILED_FIELD_COUNT.to_string(),
        ),
        ("source_revision", TESLAMATE_V4_SOURCE_REVISION.to_owned()),
        (
            "migration_set_sha256",
            TESLAMATE_V4_MIGRATION_SET_SHA256.to_owned(),
        ),
        (
            "car_settings_slice_sha256",
            thp2_2_car_settings_slice_sha256(),
        ),
        (
            "settings_slice_sha256",
            thp2_2_global_settings_slice_sha256(),
        ),
        (
            "cars_efficiency_encoding",
            THP2_2_CARS_EFFICIENCY_ENCODING.to_owned(),
        ),
        (
            "fixed_numeric_encoding",
            THP2_2_FIXED_NUMERIC_ENCODING.to_owned(),
        ),
        (
            "drives_float_encoding",
            THP2_2_DRIVES_FLOAT_ENCODING.to_owned(),
        ),
        (
            "positions_odometer_encoding",
            THP2_2_POSITIONS_ODOMETER_ENCODING.to_owned(),
        ),
        (
            "positions_relation_scope",
            THP2_2_POSITIONS_RELATION_SCOPE.to_owned(),
        ),
        (
            "charging_boolean_encoding",
            THP2_2_CHARGING_TRI_STATE_BOOL_ENCODING.to_owned(),
        ),
        (
            "charges_relation_scope",
            THP2_2_CHARGES_RELATION_SCOPE.to_owned(),
        ),
        ("cars_slice_sha256", thp2_2_cars_slice_sha256()),
        ("drives_slice_sha256", thp2_2_drives_slice_sha256()),
        ("positions_slice_sha256", thp2_2_positions_slice_sha256()),
        (
            "charging_processes_slice_sha256",
            thp2_2_charging_processes_slice_sha256(),
        ),
        ("charges_slice_sha256", thp2_2_charges_slice_sha256()),
        ("address_slice_sha256", thp2_2_address_slice_sha256()),
        ("geofence_slice_sha256", thp2_2_geofence_slice_sha256()),
        (
            "postgres_timestamp_encoding",
            THP2_2_POSTGRES_TIMESTAMP_ENCODING.to_owned(),
        ),
        (
            "postgres_timestamp_0_encoding",
            THP2_2_POSTGRES_TIMESTAMP_0_ENCODING.to_owned(),
        ),
        ("states_slice_sha256", thp2_2_states_slice_sha256()),
        ("updates_slice_sha256", thp2_2_updates_slice_sha256()),
        ("reconciliation", "not_run".to_owned()),
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
    let mut expected_metadata = expected_metadata
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect::<Vec<_>>();
    expected_metadata.sort_unstable();
    let mut metadata_statement = connection
        .prepare("SELECT key, value FROM hub_pack_metadata ORDER BY key")
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let actual_metadata = metadata_statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(ProjectionPackError::IntegrityCheck)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if actual_metadata != expected_metadata {
        return Err(invalid("schema 2.2 metadata key/value set is invalid"));
    }
    let encoded_row_count: i64 = connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM cars)
                + (SELECT COUNT(*) FROM car_settings)
                + (SELECT COUNT(*) FROM addresses)
                + (SELECT COUNT(*) FROM geofences)
                + (SELECT COUNT(*) FROM global_settings)
                + (SELECT COUNT(*) FROM drives)
                + (SELECT COUNT(*) FROM positions)
                + (SELECT COUNT(*) FROM charging_processes)
                + (SELECT COUNT(*) FROM charges)
                + (SELECT COUNT(*) FROM states)
                + (SELECT COUNT(*) FROM updates)",
            [],
            |row| row.get(0),
        )
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if u64::try_from(encoded_row_count).ok() != Some(row_count) {
        return Err(invalid("schema 2.2 row_count does not match SQLite rows"));
    }
    ensure_foreign_keys_clean(&connection)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if integrity != "ok" {
        return Err(ProjectionPackError::IntegrityFailure);
    }
    Ok(())
}

fn verify_projection_table_layout(
    connection: &Connection,
    table: &str,
    expected_without_rowid: bool,
    expected_columns: &[(&str, &str, bool, bool)],
) -> Result<(), ProjectionPackError> {
    let mut table_statement = connection
        .prepare("PRAGMA table_list")
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let table_flags = table_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(ProjectionPackError::IntegrityCheck)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let Some((_, actual_without_rowid, actual_strict)) =
        table_flags.into_iter().find(|(name, _, _)| name == table)
    else {
        return Err(invalid(format!("schema 2.2 {table} table is missing")));
    };
    if actual_without_rowid != i64::from(expected_without_rowid) || actual_strict != 1 {
        return Err(invalid(format!(
            "schema 2.2 {table} table flags are invalid"
        )));
    }

    let mut statement = connection
        .prepare(&format!("PRAGMA table_xinfo('{table}')"))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let actual_columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(ProjectionPackError::IntegrityCheck)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let expected_columns = expected_columns
        .iter()
        .map(|(name, declared_type, not_null, primary_key)| {
            (
                (*name).to_owned(),
                (*declared_type).to_owned(),
                i64::from(*not_null),
                i64::from(*primary_key),
                0,
            )
        })
        .collect::<Vec<_>>();
    if actual_columns != expected_columns {
        return Err(invalid(format!(
            "schema 2.2 {table} column layout is invalid"
        )));
    }
    Ok(())
}

fn verify_projection_foreign_keys(
    connection: &Connection,
    table: &str,
    expected_foreign_keys: &[(&str, &str, &str)],
) -> Result<(), ProjectionPackError> {
    let mut statement = connection
        .prepare(&format!("PRAGMA foreign_key_list('{table}')"))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let mut values = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(ProjectionPackError::IntegrityCheck)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProjectionPackError::IntegrityCheck)?;
    values.sort_unstable();
    let mut expected = expected_foreign_keys
        .iter()
        .map(|(target_table, from_column, to_column)| {
            (
                (*target_table).to_owned(),
                (*from_column).to_owned(),
                (*to_column).to_owned(),
            )
        })
        .collect::<Vec<_>>();
    expected.sort_unstable();
    if values != expected {
        return Err(invalid(format!(
            "schema 2.2 {table} foreign-key layout is invalid"
        )));
    }
    Ok(())
}

fn verify_projection_table_ddl(
    connection: &Connection,
    table: &str,
    expected_sql: &str,
) -> Result<(), ProjectionPackError> {
    let actual_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if normalize_sqlite_ddl(&actual_sql) != normalize_sqlite_ddl(expected_sql) {
        return Err(invalid(format!("schema 2.2 {table} DDL is invalid")));
    }
    Ok(())
}

fn normalize_sqlite_ddl(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<String>()
        .to_ascii_lowercase()
}

fn ensure_foreign_keys_clean(connection: &Connection) -> Result<(), ProjectionPackError> {
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let mut rows = statement
        .query([])
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if rows
        .next()
        .map_err(ProjectionPackError::IntegrityCheck)?
        .is_some()
    {
        return Err(ProjectionPackError::IntegrityFailure);
    }
    Ok(())
}

fn thp2_2_geofence_slice_sha256() -> String {
    THP2_2_GEOFENCE_SLICE_SHA256.to_owned()
}

fn thp2_2_car_settings_slice_sha256() -> String {
    THP2_2_CAR_SETTINGS_SLICE_SHA256.to_owned()
}

fn thp2_2_global_settings_slice_sha256() -> String {
    THP2_2_GLOBAL_SETTINGS_SLICE_SHA256.to_owned()
}

fn thp2_2_cars_slice_sha256() -> String {
    THP2_2_CARS_SLICE_SHA256.to_owned()
}

fn thp2_2_address_slice_sha256() -> String {
    THP2_2_ADDRESS_SLICE_SHA256.to_owned()
}

fn thp2_2_drives_slice_sha256() -> String {
    THP2_2_DRIVES_SLICE_SHA256.to_owned()
}

fn thp2_2_positions_slice_sha256() -> String {
    THP2_2_POSITIONS_SLICE_SHA256.to_owned()
}

fn thp2_2_charging_processes_slice_sha256() -> String {
    THP2_2_CHARGING_PROCESSES_SLICE_SHA256.to_owned()
}

fn thp2_2_charges_slice_sha256() -> String {
    THP2_2_CHARGES_SLICE_SHA256.to_owned()
}

fn thp2_2_states_slice_sha256() -> String {
    THP2_2_STATES_SLICE_SHA256.to_owned()
}

fn thp2_2_updates_slice_sha256() -> String {
    THP2_2_UPDATES_SLICE_SHA256.to_owned()
}

fn insert_metadata_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    request: &ProjectionPackRequestV2_2<'_>,
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let values = [
        ("protocol", "teslatlas-sync".to_owned()),
        ("pack_format", "hub_projection_sqlite".to_owned()),
        ("schema_major", HUB_PROJECTION_SCHEMA_V3.major.to_string()),
        ("schema_minor", HUB_PROJECTION_SCHEMA_V3.minor.to_string()),
        ("pack_id", request.pack_id.to_string()),
        ("snapshot_id", request.snapshot_id.to_string()),
        ("ordinal", request.ordinal.to_string()),
        ("mode", "full_snapshot".to_owned()),
        ("schema_support", "full_snapshot_only".to_owned()),
        ("publication_scope", "local_validation_only".to_owned()),
        ("ledger_state", "draft_blocked".to_owned()),
        (
            "ledger_slice",
            "settings+car_settings+cars+drives+positions+charging_processes+charges+addresses+geofences+states+updates".to_owned(),
        ),
        ("mapped_fields", THP2_2_MAPPED_FIELD_COUNT.to_string()),
        (
            "unreconciled_fields",
            THP2_2_UNRECONCILED_FIELD_COUNT.to_string(),
        ),
        ("source_revision", TESLAMATE_V4_SOURCE_REVISION.to_owned()),
        (
            "migration_set_sha256",
            TESLAMATE_V4_MIGRATION_SET_SHA256.to_owned(),
        ),
        (
            "car_settings_slice_sha256",
            thp2_2_car_settings_slice_sha256(),
        ),
        (
            "settings_slice_sha256",
            thp2_2_global_settings_slice_sha256(),
        ),
        (
            "cars_efficiency_encoding",
            THP2_2_CARS_EFFICIENCY_ENCODING.to_owned(),
        ),
        (
            "fixed_numeric_encoding",
            THP2_2_FIXED_NUMERIC_ENCODING.to_owned(),
        ),
        (
            "drives_float_encoding",
            THP2_2_DRIVES_FLOAT_ENCODING.to_owned(),
        ),
        (
            "positions_odometer_encoding",
            THP2_2_POSITIONS_ODOMETER_ENCODING.to_owned(),
        ),
        (
            "positions_relation_scope",
            THP2_2_POSITIONS_RELATION_SCOPE.to_owned(),
        ),
        (
            "charging_boolean_encoding",
            THP2_2_CHARGING_TRI_STATE_BOOL_ENCODING.to_owned(),
        ),
        (
            "charges_relation_scope",
            THP2_2_CHARGES_RELATION_SCOPE.to_owned(),
        ),
        ("cars_slice_sha256", thp2_2_cars_slice_sha256()),
        ("drives_slice_sha256", thp2_2_drives_slice_sha256()),
        ("positions_slice_sha256", thp2_2_positions_slice_sha256()),
        (
            "charging_processes_slice_sha256",
            thp2_2_charging_processes_slice_sha256(),
        ),
        ("charges_slice_sha256", thp2_2_charges_slice_sha256()),
        ("address_slice_sha256", thp2_2_address_slice_sha256()),
        ("geofence_slice_sha256", thp2_2_geofence_slice_sha256()),
        (
            "postgres_timestamp_encoding",
            THP2_2_POSTGRES_TIMESTAMP_ENCODING.to_owned(),
        ),
        (
            "postgres_timestamp_0_encoding",
            THP2_2_POSTGRES_TIMESTAMP_0_ENCODING.to_owned(),
        ),
        ("states_slice_sha256", thp2_2_states_slice_sha256()),
        ("updates_slice_sha256", thp2_2_updates_slice_sha256()),
        ("reconciliation", "not_run".to_owned()),
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

fn insert_states_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionStateV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO states (id, car_id, state, start_date_pg_us, end_date_pg_us)\n             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.state.as_str(),
                row.start_date_pg_us,
                row.end_date_pg_us,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_updates_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionUpdateV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO updates (id, car_id, start_date_pg_us, end_date_pg_us, version)\n             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.start_date_pg_us,
                row.end_date_pg_us,
                row.version,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_addresses_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionAddressV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO addresses (
                id, display_name, latitude_e6, latitude_e6_is_nan, longitude_e6,
                longitude_e6_is_nan, name, house_number, road,
                neighbourhood, city, county, postcode, state, state_district, country,
                inserted_at_pg_us, updated_at_pg_us, osm_id, osm_type
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                ?17, ?18, ?19, ?20
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (latitude_e6, latitude_e6_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.latitude_e6);
        let (longitude_e6, longitude_e6_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.longitude_e6);
        statement
            .execute(params![
                row.id,
                row.display_name,
                latitude_e6,
                latitude_e6_is_nan,
                longitude_e6,
                longitude_e6_is_nan,
                row.name,
                row.house_number,
                row.road,
                row.neighbourhood,
                row.city,
                row.county,
                row.postcode,
                row.state,
                row.state_district,
                row.country,
                row.inserted_at_pg_us,
                row.updated_at_pg_us,
                row.osm_id,
                row.osm_type,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_geofences_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionGeofenceV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO geofences (
                id, name, latitude_e6, latitude_e6_is_nan, longitude_e6, longitude_e6_is_nan,
                radius, billing_type, cost_per_unit_e4, cost_per_unit_e4_is_nan, session_fee_e2,
                session_fee_e2_is_nan, inserted_at_pg_us, updated_at_pg_us
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (latitude_e6, latitude_e6_is_nan) = row.latitude_e6.sqlite_parts();
        let (longitude_e6, longitude_e6_is_nan) = row.longitude_e6.sqlite_parts();
        let (cost_per_unit_e4, cost_per_unit_e4_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.cost_per_unit_e4);
        let (session_fee_e2, session_fee_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.session_fee_e2);
        statement
            .execute(params![
                row.id,
                row.name,
                latitude_e6,
                latitude_e6_is_nan,
                longitude_e6,
                longitude_e6_is_nan,
                row.radius,
                row.billing_type.as_str(),
                cost_per_unit_e4,
                cost_per_unit_e4_is_nan,
                session_fee_e2,
                session_fee_e2_is_nan,
                row.inserted_at_pg_us,
                row.updated_at_pg_us,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_drives_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionDriveV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|value| value.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO drives (
                id, car_id, start_date_pg_us, end_date_pg_us, start_position_id,
                end_position_id, start_address_id, end_address_id, start_geofence_id,
                end_geofence_id, outside_temp_avg_e1, outside_temp_avg_e1_is_nan,
                inside_temp_avg_e1, inside_temp_avg_e1_is_nan, speed_max, power_max,
                power_min, start_ideal_range_km_e2, start_ideal_range_km_e2_is_nan,
                end_ideal_range_km_e2, end_ideal_range_km_e2_is_nan,
                start_rated_range_km_e2, start_rated_range_km_e2_is_nan,
                end_rated_range_km_e2, end_rated_range_km_e2_is_nan, start_km_f64_be,
                end_km_f64_be, distance_f64_be, duration_min, ascent, descent
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                ?27, ?28, ?29, ?30, ?31
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (outside_temp_avg_e1, outside_temp_avg_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.outside_temp_avg_e1);
        let (inside_temp_avg_e1, inside_temp_avg_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.inside_temp_avg_e1);
        let (start_ideal_range_km_e2, start_ideal_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.start_ideal_range_km_e2);
        let (end_ideal_range_km_e2, end_ideal_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.end_ideal_range_km_e2);
        let (start_rated_range_km_e2, start_rated_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.start_rated_range_km_e2);
        let (end_rated_range_km_e2, end_rated_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.end_rated_range_km_e2);
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.start_date_pg_us,
                row.end_date_pg_us,
                row.start_position_id,
                row.end_position_id,
                row.start_address_id,
                row.end_address_id,
                row.start_geofence_id,
                row.end_geofence_id,
                outside_temp_avg_e1,
                outside_temp_avg_e1_is_nan,
                inside_temp_avg_e1,
                inside_temp_avg_e1_is_nan,
                row.speed_max,
                row.power_max,
                row.power_min,
                start_ideal_range_km_e2,
                start_ideal_range_km_e2_is_nan,
                end_ideal_range_km_e2,
                end_ideal_range_km_e2_is_nan,
                start_rated_range_km_e2,
                start_rated_range_km_e2_is_nan,
                end_rated_range_km_e2,
                end_rated_range_km_e2_is_nan,
                row.start_km.map(|value| value.to_be_bytes().to_vec()),
                row.end_km.map(|value| value.to_be_bytes().to_vec()),
                row.distance.map(|value| value.to_be_bytes().to_vec()),
                row.duration_min,
                row.ascent,
                row.descent,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_positions_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionPositionV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO positions (
                id, car_id, drive_id, date_pg_us, latitude_e6, latitude_e6_is_nan,
                longitude_e6, longitude_e6_is_nan, elevation, speed, power, odometer_f64_be,
                ideal_battery_range_km_e2, ideal_battery_range_km_e2_is_nan,
                est_battery_range_km_e2, est_battery_range_km_e2_is_nan,
                rated_battery_range_km_e2, rated_battery_range_km_e2_is_nan,
                battery_level, usable_battery_level, battery_heater, battery_heater_on,
                battery_heater_no_power, outside_temp_e1, outside_temp_e1_is_nan,
                inside_temp_e1, inside_temp_e1_is_nan, fan_status, driver_temp_setting_e1,
                driver_temp_setting_e1_is_nan, passenger_temp_setting_e1,
                passenger_temp_setting_e1_is_nan, is_climate_on, is_rear_defroster_on,
                is_front_defroster_on, tpms_pressure_fl_e1, tpms_pressure_fl_e1_is_nan,
                tpms_pressure_fr_e1, tpms_pressure_fr_e1_is_nan, tpms_pressure_rl_e1,
                tpms_pressure_rl_e1_is_nan, tpms_pressure_rr_e1, tpms_pressure_rr_e1_is_nan
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (latitude_e6, latitude_e6_is_nan) = row.latitude_e6.sqlite_parts();
        let (longitude_e6, longitude_e6_is_nan) = row.longitude_e6.sqlite_parts();
        let (ideal_battery_range_km_e2, ideal_battery_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.ideal_battery_range_km_e2);
        let (est_battery_range_km_e2, est_battery_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.est_battery_range_km_e2);
        let (rated_battery_range_km_e2, rated_battery_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.rated_battery_range_km_e2);
        let (outside_temp_e1, outside_temp_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.outside_temp_e1);
        let (inside_temp_e1, inside_temp_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.inside_temp_e1);
        let (driver_temp_setting_e1, driver_temp_setting_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.driver_temp_setting_e1);
        let (passenger_temp_setting_e1, passenger_temp_setting_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.passenger_temp_setting_e1);
        let (tpms_pressure_fl_e1, tpms_pressure_fl_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.tpms_pressure_fl_e1);
        let (tpms_pressure_fr_e1, tpms_pressure_fr_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.tpms_pressure_fr_e1);
        let (tpms_pressure_rl_e1, tpms_pressure_rl_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.tpms_pressure_rl_e1);
        let (tpms_pressure_rr_e1, tpms_pressure_rr_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.tpms_pressure_rr_e1);
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.drive_id,
                row.date_pg_us,
                latitude_e6,
                latitude_e6_is_nan,
                longitude_e6,
                longitude_e6_is_nan,
                row.elevation,
                row.speed,
                row.power,
                row.odometer.map(|value| value.to_be_bytes().to_vec()),
                ideal_battery_range_km_e2,
                ideal_battery_range_km_e2_is_nan,
                est_battery_range_km_e2,
                est_battery_range_km_e2_is_nan,
                rated_battery_range_km_e2,
                rated_battery_range_km_e2_is_nan,
                row.battery_level,
                row.usable_battery_level,
                bool_as_sql(row.battery_heater),
                bool_as_sql(row.battery_heater_on),
                bool_as_sql(row.battery_heater_no_power),
                outside_temp_e1,
                outside_temp_e1_is_nan,
                inside_temp_e1,
                inside_temp_e1_is_nan,
                row.fan_status,
                driver_temp_setting_e1,
                driver_temp_setting_e1_is_nan,
                passenger_temp_setting_e1,
                passenger_temp_setting_e1_is_nan,
                bool_as_sql(row.is_climate_on),
                bool_as_sql(row.is_rear_defroster_on),
                bool_as_sql(row.is_front_defroster_on),
                tpms_pressure_fl_e1,
                tpms_pressure_fl_e1_is_nan,
                tpms_pressure_fr_e1,
                tpms_pressure_fr_e1_is_nan,
                tpms_pressure_rl_e1,
                tpms_pressure_rl_e1_is_nan,
                tpms_pressure_rr_e1,
                tpms_pressure_rr_e1_is_nan,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_charging_processes_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionChargingProcessV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO charging_processes (
                id, car_id, position_id, address_id, geofence_id, start_date_pg_us,
                end_date_pg_us, charge_energy_added_e2, charge_energy_added_e2_is_nan,
                charge_energy_used_e2, charge_energy_used_e2_is_nan,
                start_ideal_range_km_e2, start_ideal_range_km_e2_is_nan,
                end_ideal_range_km_e2, end_ideal_range_km_e2_is_nan,
                start_rated_range_km_e2, start_rated_range_km_e2_is_nan,
                end_rated_range_km_e2, end_rated_range_km_e2_is_nan,
                start_battery_level, end_battery_level, duration_min,
                outside_temp_avg_e1, outside_temp_avg_e1_is_nan, cost_e2, cost_e2_is_nan
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (charge_energy_added_e2, charge_energy_added_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.charge_energy_added_e2);
        let (charge_energy_used_e2, charge_energy_used_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.charge_energy_used_e2);
        let (start_ideal_range_km_e2, start_ideal_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.start_ideal_range_km_e2);
        let (end_ideal_range_km_e2, end_ideal_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.end_ideal_range_km_e2);
        let (start_rated_range_km_e2, start_rated_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.start_rated_range_km_e2);
        let (end_rated_range_km_e2, end_rated_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.end_rated_range_km_e2);
        let (outside_temp_avg_e1, outside_temp_avg_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.outside_temp_avg_e1);
        let (cost_e2, cost_e2_is_nan) = optional_fixed_numeric_sqlite_parts(row.cost_e2);
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.position_id,
                row.address_id,
                row.geofence_id,
                row.start_date_pg_us,
                row.end_date_pg_us,
                charge_energy_added_e2,
                charge_energy_added_e2_is_nan,
                charge_energy_used_e2,
                charge_energy_used_e2_is_nan,
                start_ideal_range_km_e2,
                start_ideal_range_km_e2_is_nan,
                end_ideal_range_km_e2,
                end_ideal_range_km_e2_is_nan,
                start_rated_range_km_e2,
                start_rated_range_km_e2_is_nan,
                end_rated_range_km_e2,
                end_rated_range_km_e2_is_nan,
                row.start_battery_level,
                row.end_battery_level,
                row.duration_min,
                outside_temp_avg_e1,
                outside_temp_avg_e1_is_nan,
                cost_e2,
                cost_e2_is_nan,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_charges_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionChargeV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO charges (
                id, charging_process_id, date_pg_us, battery_heater, battery_heater_on,
                battery_heater_no_power, battery_level, usable_battery_level,
                charge_energy_added_e2, charge_energy_added_e2_is_nan,
                charger_actual_current, charger_phases, charger_pilot_current, charger_power,
                charger_voltage, conn_charge_cable, fast_charger_present, fast_charger_brand,
                fast_charger_type, ideal_battery_range_km_e2,
                ideal_battery_range_km_e2_is_nan, rated_battery_range_km_e2,
                rated_battery_range_km_e2_is_nan, not_enough_power_to_heat, outside_temp_e1,
                outside_temp_e1_is_nan
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (charge_energy_added_e2, charge_energy_added_e2_is_nan) =
            row.charge_energy_added_e2.sqlite_parts();
        let (ideal_battery_range_km_e2, ideal_battery_range_km_e2_is_nan) =
            row.ideal_battery_range_km_e2.sqlite_parts();
        let (rated_battery_range_km_e2, rated_battery_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.rated_battery_range_km_e2);
        let (outside_temp_e1, outside_temp_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.outside_temp_e1);
        statement
            .execute(params![
                row.id,
                row.charging_process_id,
                row.date_pg_us,
                bool_as_sql(row.battery_heater),
                bool_as_sql(row.battery_heater_on),
                bool_as_sql(row.battery_heater_no_power),
                row.battery_level,
                row.usable_battery_level,
                charge_energy_added_e2,
                charge_energy_added_e2_is_nan,
                row.charger_actual_current,
                row.charger_phases,
                row.charger_pilot_current,
                row.charger_power,
                row.charger_voltage,
                row.conn_charge_cable,
                bool_as_sql(row.fast_charger_present),
                row.fast_charger_brand,
                row.fast_charger_type,
                ideal_battery_range_km_e2,
                ideal_battery_range_km_e2_is_nan,
                rated_battery_range_km_e2,
                rated_battery_range_km_e2_is_nan,
                bool_as_sql(row.not_enough_power_to_heat),
                outside_temp_e1,
                outside_temp_e1_is_nan,
            ])
            .map_err(ProjectionPackError::Insert)?;
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
    insert_cars(&transaction, &request.delta.cars, true)?;
    insert_car_settings(&transaction, &request.delta.car_settings)?;
    insert_drives(&transaction, &request.delta.drives)?;
    insert_charges(&transaction, &request.delta.charges)?;
    insert_positions(&transaction, &request.delta.positions)?;
    insert_charge_samples(&transaction, &request.delta.charge_samples)?;
    insert_states(&transaction, &request.delta.states)?;
    insert_updates(&transaction, &request.delta.updates)?;
    insert_tombstones(&transaction, &request.delta.tombstones)?;
    crate::durability_fault::check(crate::durability_fault::DurabilityFaultPoint::PackSqliteCommit)
        .map_err(ProjectionPackError::Durability)?;
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
            HUB_PROJECTION_SCHEMA_V2.sqlite_user_version(),
        )
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
        ("installation_id", delta.binding.installation_id.to_string()),
        ("account_id", delta.binding.account_id.to_string()),
        ("vehicle_id", delta.binding.vehicle_id.to_string()),
        ("generation", delta.binding.generation.to_string()),
        ("selected_car_id", delta.binding.selected_car_id.to_string()),
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
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO tombstones(entity, entity_id, car_id)
             VALUES (?1, ?2, ?3)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in source_owned_tombstones_in_canonical_order(values) {
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

fn insert_legacy_cars(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCar],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO cars (
                id, name, model, vin, firmware_version, efficiency_wh_per_km
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.name,
                normalize_tesla_model_code(&row.model),
                row.vin,
                row.firmware_version,
                row.efficiency_wh_per_km,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_legacy_drives(
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
                // The released 2.0 client accepts this legacy column only
                // when it is NULL, so it is deliberately not projected.
                Option::<i64>::None,
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

fn insert_legacy_charges(
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

fn insert_legacy_positions(
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
        let drive_id = row
            .drive_id
            .ok_or_else(|| invalid("schema 2.0 position.drive_id must be present"))?;
        statement
            .execute(params![
                row.id,
                drive_id,
                row.car_id,
                row.date_ms,
                row.latitude,
                row.longitude,
                row.speed,
                v1_position_power(row.power)?,
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

fn v1_position_power(value: Option<f64>) -> Result<Option<i64>, ProjectionPackError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !value.is_finite()
        || value.fract() != 0.0
        || value < i64::MIN as f64
        || value >= -(i64::MIN as f64)
    {
        return Err(invalid("schema 2.0 position.power must be an integer"));
    }
    Ok(Some(value as i64))
}

fn insert_global_settings_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionGlobalSettingsV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO global_settings (
                id, unit_of_length, unit_of_temperature, unit_of_pressure, preferred_range,
                base_url, grafana_url, language, theme_mode, inserted_at_pg_us, updated_at_pg_us
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.unit_of_length.as_str(),
                row.unit_of_temperature.as_str(),
                row.unit_of_pressure.as_str(),
                row.preferred_range.as_str(),
                row.base_url,
                row.grafana_url,
                row.language,
                row.theme_mode,
                row.inserted_at_pg_us,
                row.updated_at_pg_us,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_car_settings_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCarSettingsV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO car_settings (
                id, suspend_min, suspend_after_idle_min, req_not_unlocked,
                free_supercharging, use_streaming_api, enabled, lfp_battery
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.suspend_min,
                row.suspend_after_idle_min,
                row.req_not_unlocked,
                row.free_supercharging,
                row.use_streaming_api,
                row.enabled,
                row.lfp_battery,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_cars_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCarV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO cars (
                id, eid, vid, vin, name, model, efficiency, trim_badging,
                marketing_name, exterior_color, wheel_type, spoiler_type,
                display_priority, inserted_at_pg_us, updated_at_pg_us, settings_id
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let efficiency_bits = row
            .efficiency
            .map(|value| value.to_bits().to_be_bytes().to_vec());
        statement
            .execute(params![
                row.id,
                row.eid,
                row.vid,
                row.vin,
                row.name,
                row.model,
                efficiency_bits,
                row.trim_badging,
                row.marketing_name,
                row.exterior_color,
                row.wheel_type,
                row.spoiler_type,
                row.display_priority,
                row.inserted_at_pg_us,
                row.updated_at_pg_us,
                row.settings_id,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_cars(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCar],
    include_settings: bool,
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
    if !include_settings {
        return Ok(());
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
    compress_file_with_workers(source_path, destination_path, compression_worker_count())
}

const MAX_COMPRESSION_WORKERS: usize = 4;

fn compression_worker_count() -> u32 {
    let available = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    compression_worker_count_for(available)
}

fn compression_worker_count_for(available: usize) -> u32 {
    available.clamp(1, MAX_COMPRESSION_WORKERS) as u32
}

fn compress_file_with_workers(
    source_path: &Path,
    destination_path: &Path,
    workers: u32,
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
    encoder
        .multithread(workers)
        .map_err(ProjectionPackError::Compress)?;
    io::copy(&mut source, &mut encoder).map_err(ProjectionPackError::Compress)?;
    crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::PackCompressedWrite,
    )
    .map_err(ProjectionPackError::Durability)?;
    let (file, digest, bytes) = encoder
        .finish()
        .map_err(ProjectionPackError::Compress)?
        .finish();
    crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::PackCompressedFsync,
    )
    .map_err(ProjectionPackError::Durability)?;
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

struct ImmutablePublication {
    ownership: ProjectionPackOwnership,
    cleanup_state: ProjectionPackCleanupState,
}

fn publish_immutable(
    temporary: &mut StagedFile,
    final_path: &Path,
    metadata: &TransportPack,
    limits: ProtocolLimits,
) -> Result<ImmutablePublication, ProjectionPackError> {
    let temporary_path = temporary.path().to_path_buf();
    fs::set_permissions(
        &temporary_path,
        fs::Permissions::from_mode(SHARED_IMMUTABLE_PACK_MODE),
    )
    .map_err(|source| ProjectionPackError::Publish {
        path: temporary_path.to_path_buf(),
        source,
    })?;
    File::open(&temporary_path)
        .and_then(|file| file.sync_all())
        .map_err(|source| ProjectionPackError::Publish {
            path: temporary_path.to_path_buf(),
            source,
        })?;
    crate::durability_fault::check(crate::durability_fault::DurabilityFaultPoint::PackFinalInstall)
        .map_err(ProjectionPackError::Durability)?;
    let ownership = match fs::hard_link(&temporary_path, final_path) {
        Ok(()) => ProjectionPackOwnership::Created,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            verify_file(metadata, final_path, limits)
                .map(|_| ProjectionPackOwnership::ReusedExisting)?
        }
        Err(source) => Err(ProjectionPackError::Publish {
            path: final_path.to_path_buf(),
            source,
        })?,
    };
    // A prior attempt may have installed this exact immutable name and then
    // failed before syncing its parent directory. Reuse proves the bytes, not
    // the directory entry's durability, so both paths must cross the same
    // checkpoint and sync before publication can be reported complete.
    crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::PackFinalDirectoryFsync,
    )
    .map_err(ProjectionPackError::Durability)?;
    sync_parent_directory(final_path)?;
    // Once a final content name exists (or a verified identical object was
    // reused), an error in staging cleanup is a restart-repairable orphan.
    // Do not let `Drop` erase the evidence or pretend its directory entry was
    // durably removed.
    temporary.retain_for_repair();
    // The final immutable hard link and its parent have been synced.  Remove
    // the now-public staging name and sync that namespace too, so a normal
    // completed publication never leaves a 0640 temporary file behind.
    if let Err(source) = crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::PackStagingUnlink,
    ) {
        tracing::warn!(%source, path = %temporary_path.display(), "pack committed with staging cleanup pending");
        return Ok(ImmutablePublication {
            ownership,
            cleanup_state: ProjectionPackCleanupState::PendingStartupRepair,
        });
    }
    if let Err(source) = fs::remove_file(&temporary_path) {
        tracing::warn!(%source, path = %temporary_path.display(), "pack committed with staging cleanup pending");
        return Ok(ImmutablePublication {
            ownership,
            cleanup_state: ProjectionPackCleanupState::PendingStartupRepair,
        });
    }
    temporary.mark_removed();
    if let Err(source) = crate::durability_fault::check(
        crate::durability_fault::DurabilityFaultPoint::PackStagingDirectoryFsync,
    ) {
        tracing::warn!(%source, path = %temporary_path.display(), "pack committed with staging directory sync pending");
        return Ok(ImmutablePublication {
            ownership,
            cleanup_state: ProjectionPackCleanupState::PendingStartupRepair,
        });
    }
    if let Err(source) = sync_parent_directory(&temporary_path) {
        tracing::warn!(%source, path = %temporary_path.display(), "pack committed with staging directory sync pending");
        return Ok(ImmutablePublication {
            ownership,
            cleanup_state: ProjectionPackCleanupState::PendingStartupRepair,
        });
    }
    Ok(ImmutablePublication {
        ownership,
        cleanup_state: ProjectionPackCleanupState::Complete,
    })
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

fn require_unique_signed_i32(
    ids: &mut HashSet<i64>,
    value: i32,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if !ids.insert(i64::from(value)) {
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

fn validate_timestamp_0_pg_us(value: i64, field: &str) -> Result<(), ProjectionPackError> {
    let is_infinity = matches!(value, i64::MIN | i64::MAX);
    let is_finite_second = (POSTGRES_TIMESTAMP_FINITE_MIN_US
        ..POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US)
        .contains(&value)
        && value.rem_euclid(1_000_000) == 0;
    if !is_infinity && !is_finite_second {
        return Err(invalid(format!(
            "{field} is outside the PostgreSQL timestamp(0) source domain"
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

fn validate_bounded_i64(
    value: i64,
    minimum: i64,
    maximum: i64,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if !(minimum..=maximum).contains(&value) {
        return Err(invalid(format!(
            "{field} is outside its pinned source range"
        )));
    }
    Ok(())
}

fn validate_fixed_numeric_v2_2(
    value: ProjectionFixedNumericV2_2,
    minimum: i64,
    maximum: i64,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if let ProjectionFixedNumericV2_2::Finite(value) = value {
        validate_bounded_i64(value, minimum, maximum, field)?;
    }
    Ok(())
}

fn validate_optional_fixed_numeric_v2_2(
    value: Option<ProjectionFixedNumericV2_2>,
    minimum: i64,
    maximum: i64,
    field: &str,
) -> Result<(), ProjectionPackError> {
    if let Some(value) = value {
        validate_fixed_numeric_v2_2(value, minimum, maximum, field)?;
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

fn validate_required_text_with_source_width(
    value: &str,
    maximum_characters: usize,
    field: &str,
) -> Result<(), ProjectionPackError> {
    // PostgreSQL `varchar(n) NOT NULL` accepts the empty string. The Rust
    // field itself represents the non-null part of the source contract.
    validate_optional_text(Some(value), field)?;
    if value.chars().count() > maximum_characters {
        return Err(invalid(format!("{field} exceeds its pinned source width")));
    }
    Ok(())
}

fn validate_optional_text(value: Option<&str>, field: &str) -> Result<(), ProjectionPackError> {
    if value.is_some_and(|value| value.len() > MAX_TEXT_BYTES || value.as_bytes().contains(&0)) {
        return Err(invalid(format!("{field} is unsafe or too large")));
    }
    Ok(())
}

fn validate_optional_text_with_source_width(
    value: Option<&str>,
    maximum_characters: usize,
    field: &str,
) -> Result<(), ProjectionPackError> {
    validate_optional_text(value, field)?;
    if value.is_some_and(|value| value.chars().count() > maximum_characters) {
        return Err(invalid(format!("{field} exceeds its pinned source width")));
    }
    Ok(())
}

struct StagedFile {
    path: PathBuf,
    cleanup_on_drop: bool,
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
                    return Ok(Self {
                        path,
                        cleanup_on_drop: true,
                    });
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

    fn retain_for_repair(&mut self) {
        self.cleanup_on_drop = false;
    }

    fn mark_removed(&mut self) {
        self.cleanup_on_drop = false;
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = fs::remove_file(&self.path);
        }
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
    #[error("projection durability checkpoint failed: {0}")]
    Durability(io::Error),
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
        CursorClaims, LINEAGE_PROTOCOL_V2, LineageBase, LineageCapability, LineageDelta,
        LineageManifestV2, OpaqueCursor, PROTOCOL_V1,
    };
    use crate::teslamate_projection::TeslaMateGeofencePhysicalV2_2;

    use super::*;

    #[test]
    fn owner_api_model_codes_are_normalized_like_teslamate() {
        assert_eq!(normalize_tesla_model_code("model3"), "3");
        assert_eq!(normalize_tesla_model_code("models2"), "S");
        assert_eq!(normalize_tesla_model_code("modely"), "Y");
        assert_eq!(normalize_tesla_model_code("Model 3"), "3");
    }

    #[test]
    fn model_3_base_trim_uses_the_vin_model_year() {
        assert_eq!(
            derive_tesla_marketing_name("3", Some("50"), Some("model3"), Some("5YJ3E1EA7NF000001")),
            Some("RWD".to_owned())
        );
        assert_eq!(
            derive_tesla_marketing_name("3", Some("50"), Some("model3"), Some("5YJ3E1EA7MF000001")),
            Some("SR+".to_owned())
        );
        assert_eq!(
            derive_tesla_marketing_name("3", Some("50"), Some("model3"), None),
            Some("SR+".to_owned())
        );
    }

    #[test]
    fn teslamate_suspend_default_matches_creation_conditions() {
        assert_eq!(
            teslamate_suspend_min_default(Some("3"), Some("74D"), None),
            Some(12)
        );
        assert_eq!(
            teslamate_suspend_min_default(Some("Y"), None, None),
            Some(12)
        );
        assert_eq!(
            teslamate_suspend_min_default(Some("S"), None, None),
            Some(12)
        );
        assert_eq!(
            teslamate_suspend_min_default(Some("X"), Some("100D"), Some("LR")),
            Some(12)
        );
        assert_eq!(
            teslamate_suspend_min_default(Some("S"), Some("100D"), None),
            Some(21)
        );
        assert_eq!(
            teslamate_suspend_min_default(Some("Cybertruck"), None, None),
            None
        );
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
    fn fault_matrix_never_leaves_a_partial_content_pack() {
        use crate::durability_fault::{DurabilityFaultPoint, inject};

        let cases = [
            (DurabilityFaultPoint::PackSqliteCommit, false, false, false),
            (
                DurabilityFaultPoint::PackCompressedWrite,
                false,
                false,
                false,
            ),
            (
                DurabilityFaultPoint::PackCompressedFsync,
                false,
                false,
                false,
            ),
            (DurabilityFaultPoint::PackFinalInstall, false, false, false),
            (
                DurabilityFaultPoint::PackFinalDirectoryFsync,
                true,
                false,
                false,
            ),
            (DurabilityFaultPoint::PackStagingUnlink, true, true, true),
            (
                DurabilityFaultPoint::PackStagingDirectoryFsync,
                true,
                false,
                true,
            ),
        ];
        for (point, expect_final, expect_staging, expect_cleanup_pending) in cases {
            let temporary = tempfile::tempdir().expect("fault store");
            let source = snapshot();
            let _fault = inject(point);
            let result =
                ProjectionPackWriter::new(temporary.path()).write_full_snapshot(&request(&source));
            if expect_cleanup_pending {
                let built = result.expect("post-publication cleanup is a successful pack receipt");
                assert_eq!(
                    built.cleanup_state(),
                    ProjectionPackCleanupState::PendingStartupRepair
                );
            } else {
                let error = result.expect_err("pre-publication durability point must fail");
                assert!(
                    error.to_string().contains("durability fault"),
                    "typed fault for {point:?}: {error}"
                );
            }

            let content_directory = temporary.path().join("sha256");
            let content = fs::read_dir(&content_directory)
                .map(|entries| {
                    entries
                        .map(|entry| entry.expect("content entry").path())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            assert_eq!(
                !content.is_empty(),
                expect_final,
                "final namespace outcome for {point:?}"
            );
            for path in content {
                let bytes = fs::read(&path).expect("read completed content object");
                let name = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .expect("UTF-8 content name");
                let digest = name
                    .strip_suffix(".sqlite.zst")
                    .expect("canonical content suffix");
                assert_eq!(hex::encode(Sha256::digest(&bytes)), digest);
                assert!(!bytes.is_empty());
            }

            let staging_directory = temporary.path().join(".staging");
            let staging_count = fs::read_dir(staging_directory)
                .map(|entries| entries.count())
                .unwrap_or(0);
            assert_eq!(
                staging_count > 0,
                expect_staging,
                "restart-visible staging outcome for {point:?}"
            );
        }
    }

    #[test]
    fn existing_content_retry_repeats_the_final_directory_sync() {
        use crate::durability_fault::{DurabilityFaultPoint, inject};

        let temporary = tempfile::tempdir().expect("retry store");
        let source = snapshot();
        {
            let _fault = inject(DurabilityFaultPoint::PackFinalDirectoryFsync);
            ProjectionPackWriter::new(temporary.path())
                .write_full_snapshot(&request(&source))
                .expect_err("first directory sync fault");
        }
        {
            let _fault = inject(DurabilityFaultPoint::PackFinalDirectoryFsync);
            ProjectionPackWriter::new(temporary.path())
                .write_full_snapshot(&request(&source))
                .expect_err("verified reuse still repeats directory sync");
        }
        let built = ProjectionPackWriter::new(temporary.path())
            .write_full_snapshot(&request(&source))
            .expect("normal retry durably converges");
        assert_eq!(built.ownership(), ProjectionPackOwnership::ReusedExisting);
        assert_eq!(built.cleanup_state(), ProjectionPackCleanupState::Complete);
    }

    fn snapshot_v2_2() -> ProjectionSnapshotV2_2 {
        ProjectionSnapshotV2_2 {
            global_settings: vec![ProjectionGlobalSettingsV2_2 {
                id: i64::MIN,
                unit_of_length: ProjectionUnitOfLengthV2_2::Kilometers,
                unit_of_temperature: ProjectionUnitOfTemperatureV2_2::Celsius,
                unit_of_pressure: ProjectionUnitOfPressureV2_2::Bar,
                preferred_range: ProjectionPreferredRangeV2_2::Rated,
                base_url: Some("https://teslamate.example".into()),
                grafana_url: None,
                language: String::new(),
                theme_mode: "system".into(),
                inserted_at_pg_us: i64::MIN,
                updated_at_pg_us: i64::MAX,
            }],
            cars: vec![ProjectionCarV2_2 {
                id: 10,
                eid: 101,
                vid: 201,
                vin: Some("5YJTESTVIN1234567".into()),
                name: Some("Road car".into()),
                model: Some("Model 3".into()),
                // This remains source FLOAT8, deliberately not the legacy
                // normalized Wh-per-km compatibility value.
                efficiency: Some(-0.145),
                trim_badging: Some("74D".into()),
                marketing_name: Some("LR AWD".into()),
                exterior_color: Some("Pearl White".into()),
                wheel_type: Some("Apollo".into()),
                spoiler_type: Some("None".into()),
                display_priority: i16::MIN,
                inserted_at_pg_us: 1_700_000_000_000_000,
                updated_at_pg_us: 1_700_000_100_000_000,
                settings_id: 500,
            }],
            car_settings: vec![ProjectionCarSettingsV2_2 {
                id: 500,
                suspend_min: i32::MIN,
                suspend_after_idle_min: i32::MAX,
                req_not_unlocked: true,
                free_supercharging: false,
                use_streaming_api: true,
                enabled: true,
                lfp_battery: false,
            }],
            addresses: vec![
                ProjectionAddressV2_2 {
                    id: 100,
                    display_name: Some("Home, London".into()),
                    latitude_e6: Some(ProjectionFixedNumericV2_2::Finite(51_500_123)),
                    longitude_e6: Some(ProjectionFixedNumericV2_2::Finite(-123_456)),
                    name: Some("Home".into()),
                    house_number: Some("1".into()),
                    road: Some("Strawberry Road".into()),
                    neighbourhood: Some("Westminster".into()),
                    city: Some("London".into()),
                    county: Some("Greater London".into()),
                    postcode: Some("SW1A 1AA".into()),
                    state: Some("England".into()),
                    state_district: Some("London".into()),
                    country: Some("United Kingdom".into()),
                    inserted_at_pg_us: 1_700_000_000_000_000,
                    updated_at_pg_us: 1_700_000_100_000_000,
                    osm_id: Some(-42),
                    osm_type: Some("node".into()),
                },
                ProjectionAddressV2_2 {
                    id: 101,
                    display_name: Some("Work, London".into()),
                    latitude_e6: Some(ProjectionFixedNumericV2_2::NaN),
                    longitude_e6: None,
                    name: Some("Work".into()),
                    house_number: None,
                    road: None,
                    neighbourhood: None,
                    city: None,
                    county: None,
                    postcode: None,
                    state: None,
                    state_district: None,
                    country: None,
                    inserted_at_pg_us: 1_700_000_200_000_000,
                    updated_at_pg_us: 1_700_000_300_000_000,
                    osm_id: None,
                    osm_type: None,
                },
            ],
            geofences: vec![
                TeslaMateGeofencePhysicalV2_2 {
                    id: 200,
                    name: "Home".into(),
                    latitude_e6: ProjectionFixedNumericV2_2::Finite(0),
                    longitude_e6: ProjectionFixedNumericV2_2::NaN,
                    radius: i16::MIN,
                    billing_type: GeofenceBillingType::PerKwh,
                    cost_per_unit_e4: Some(ProjectionFixedNumericV2_2::Finite(3_000)),
                    session_fee_e2: Some(ProjectionFixedNumericV2_2::NaN),
                    inserted_at_pg_us: 1_700_000_000_000_000,
                    updated_at_pg_us: 1_700_000_100_000_000,
                }
                .into(),
                TeslaMateGeofencePhysicalV2_2 {
                    id: 201,
                    name: "Work".into(),
                    latitude_e6: ProjectionFixedNumericV2_2::NaN,
                    longitude_e6: ProjectionFixedNumericV2_2::Finite(-110_000),
                    radius: i16::MAX,
                    billing_type: GeofenceBillingType::PerMinute,
                    cost_per_unit_e4: None,
                    session_fee_e2: None,
                    inserted_at_pg_us: 1_700_000_200_000_000,
                    updated_at_pg_us: 1_700_000_300_000_000,
                }
                .into(),
            ],
            drives: vec![ProjectionDriveV2_2 {
                id: 20,
                car_id: 10,
                start_date_pg_us: i64::MAX,
                end_date_pg_us: Some(i64::MIN),
                start_position_id: Some(i32::MIN),
                end_position_id: Some(i32::MAX),
                start_address_id: Some(100),
                end_address_id: Some(101),
                start_geofence_id: Some(200),
                end_geofence_id: Some(201),
                outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::NaN),
                inside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
                speed_max: Some(i16::MIN),
                power_max: Some(i16::MAX),
                power_min: Some(i16::MIN),
                start_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
                end_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
                start_rated_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
                end_rated_range_km_e2: None,
                start_km: Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits())),
                end_km: Some(ProjectionFloat64BitsV2_2(0x7ff8_0000_0000_0042)),
                distance: Some(ProjectionFloat64BitsV2_2(f64::INFINITY.to_bits())),
                duration_min: Some(i16::MIN),
                ascent: Some(i16::MAX),
                descent: Some(i16::MIN),
            }],
            positions: vec![ProjectionPositionV2_2 {
                id: 30,
                car_id: 10,
                drive_id: Some(20),
                date_pg_us: 1_700_000_030_123_456,
                latitude_e6: ProjectionFixedNumericV2_2::Finite(51_505_000),
                longitude_e6: ProjectionFixedNumericV2_2::Finite(-105_000),
                elevation: Some(i16::MIN),
                speed: Some(i16::MAX),
                power: Some(i16::MIN),
                odometer: Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits())),
                ideal_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
                est_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
                rated_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
                battery_level: Some(i16::MIN),
                usable_battery_level: Some(i16::MAX),
                battery_heater: Some(false),
                battery_heater_on: Some(true),
                battery_heater_no_power: None,
                outside_temp_e1: Some(ProjectionFixedNumericV2_2::NaN),
                inside_temp_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
                fan_status: Some(i32::MIN),
                driver_temp_setting_e1: None,
                passenger_temp_setting_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
                is_climate_on: Some(true),
                is_rear_defroster_on: Some(false),
                is_front_defroster_on: None,
                tpms_pressure_fl_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
                tpms_pressure_fr_e1: Some(ProjectionFixedNumericV2_2::NaN),
                tpms_pressure_rl_e1: None,
                tpms_pressure_rr_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
            }],
            charging_processes: vec![ProjectionChargingProcessV2_2 {
                id: 40,
                car_id: 10,
                position_id: 30,
                address_id: Some(100),
                geofence_id: Some(200),
                start_date_pg_us: i64::MIN,
                end_date_pg_us: Some(i64::MAX),
                charge_energy_added_e2: Some(ProjectionFixedNumericV2_2::NaN),
                charge_energy_used_e2: None,
                start_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
                end_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
                start_rated_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
                end_rated_range_km_e2: None,
                start_battery_level: Some(i16::MIN),
                end_battery_level: Some(i16::MAX),
                duration_min: Some(i16::MIN),
                outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
                cost_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
            }],
            charges: vec![ProjectionChargeV2_2 {
                id: 50,
                charging_process_id: 40,
                date_pg_us: i64::MAX,
                battery_heater: Some(false),
                battery_heater_on: Some(true),
                battery_heater_no_power: None,
                battery_level: Some(i16::MIN),
                usable_battery_level: Some(i16::MAX),
                charge_energy_added_e2: ProjectionFixedNumericV2_2::NaN,
                charger_actual_current: Some(i16::MIN),
                charger_phases: Some(i16::MAX),
                charger_pilot_current: Some(i16::MIN),
                charger_power: i16::MAX,
                charger_voltage: Some(i16::MIN),
                conn_charge_cable: Some("Type 2".into()),
                fast_charger_present: Some(false),
                fast_charger_brand: Some("Tesla".into()),
                fast_charger_type: Some("Supercharger".into()),
                ideal_battery_range_km_e2: ProjectionFixedNumericV2_2::Finite(-999_999),
                rated_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
                not_enough_power_to_heat: Some(false),
                outside_temp_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
            }],
            states: vec![ProjectionStateV2_2 {
                id: i32::MIN,
                car_id: 10,
                state: ProjectionStateStatusV2_2::Online,
                start_date_pg_us: i64::MIN,
                end_date_pg_us: None,
            }],
            updates: vec![ProjectionUpdateV2_2 {
                id: i32::MAX,
                car_id: 10,
                start_date_pg_us: i64::MAX,
                end_date_pg_us: Some(i64::MIN),
                version: Some("2026.3".into()),
            }],
        }
    }

    fn request_v2_2<'a>(snapshot: &'a ProjectionSnapshotV2_2) -> ProjectionPackRequestV2_2<'a> {
        ProjectionPackRequestV2_2 {
            pack_id: Uuid::parse_str("45454545-4545-4545-8454-454545454545").unwrap(),
            snapshot_id: Uuid::parse_str("56565656-5656-4565-8565-565656565656").unwrap(),
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
    fn schema_2_2_transport_table_vocabulary_maps_physical_charging_parent_and_child() {
        let mut process_only = snapshot_v2_2();
        process_only.drives.clear();
        process_only.positions.clear();
        process_only.charges.clear();
        process_only.states.clear();
        process_only.updates.clear();
        assert_eq!(
            tables_for_snapshot_v2_2(&process_only),
            vec![MirrorTable::Car, MirrorTable::Charge],
            "physical charging_processes map to the protocol parent vocabulary"
        );

        let mut charge_only = snapshot_v2_2();
        charge_only.drives.clear();
        charge_only.positions.clear();
        charge_only.charging_processes.clear();
        charge_only.states.clear();
        charge_only.updates.clear();
        assert_eq!(
            tables_for_snapshot_v2_2(&charge_only),
            vec![MirrorTable::Car, MirrorTable::ChargeSample],
            "physical charges map to the protocol child vocabulary independently"
        );

        let full = snapshot_v2_2();
        assert_eq!(
            tables_for_snapshot_v2_2(&full),
            vec![
                MirrorTable::Car,
                MirrorTable::Drive,
                MirrorTable::Charge,
                MirrorTable::Position,
                MirrorTable::ChargeSample,
                MirrorTable::State,
                MirrorTable::Update,
            ]
        );
    }

    /// Mirrors the released client verifier's `PRAGMA table_list` and
    /// `table_xinfo` contract. This deliberately checks more than table
    /// names: a schema-2.0 pack must not inherit any 2.1 columns or types.
    fn assert_schema_2_0_client_layout(connection: &Connection) {
        let mut table_statement = connection.prepare("PRAGMA table_list").unwrap();
        let table_flags = table_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let expected: &[(&str, bool, &[(&str, &str, bool, bool)])] = &[
            (
                "hub_pack_metadata",
                false,
                &[("key", "TEXT", true, true), ("value", "TEXT", true, false)],
            ),
            (
                "cars",
                true,
                &[
                    ("id", "INTEGER", true, true),
                    ("name", "TEXT", true, false),
                    ("model", "TEXT", true, false),
                    ("vin", "TEXT", false, false),
                    ("firmware_version", "TEXT", false, false),
                    ("efficiency_wh_per_km", "REAL", false, false),
                ],
            ),
            (
                "drives",
                true,
                &[
                    ("id", "INTEGER", true, true),
                    ("car_id", "INTEGER", true, false),
                    ("optimized_at_ms", "INTEGER", false, false),
                    ("start_date_ms", "INTEGER", true, false),
                    ("end_date_ms", "INTEGER", true, false),
                    ("distance_km", "REAL", false, false),
                    ("duration_min", "INTEGER", false, false),
                    ("efficiency", "REAL", false, false),
                    ("outside_temp_avg", "REAL", false, false),
                    ("speed_max", "INTEGER", false, false),
                    ("start_address", "TEXT", false, false),
                    ("end_address", "TEXT", false, false),
                    ("start_geofence", "TEXT", false, false),
                    ("end_geofence", "TEXT", false, false),
                    ("start_latitude", "REAL", false, false),
                    ("start_longitude", "REAL", false, false),
                    ("end_latitude", "REAL", false, false),
                    ("end_longitude", "REAL", false, false),
                    ("start_soc", "INTEGER", false, false),
                    ("end_soc", "INTEGER", false, false),
                    ("start_rated_range_km", "REAL", false, false),
                    ("end_rated_range_km", "REAL", false, false),
                ],
            ),
            (
                "charges",
                true,
                &[
                    ("id", "INTEGER", true, true),
                    ("car_id", "INTEGER", true, false),
                    ("start_date_ms", "INTEGER", true, false),
                    ("end_date_ms", "INTEGER", false, false),
                    ("charge_energy_added", "REAL", false, false),
                    ("start_battery_level", "INTEGER", false, false),
                    ("end_battery_level", "INTEGER", false, false),
                    ("duration_min", "INTEGER", false, false),
                    ("address", "TEXT", false, false),
                    ("location_name", "TEXT", false, false),
                    ("geofence", "TEXT", false, false),
                    ("is_dc", "INTEGER", false, false),
                    ("charge_rate_km_per_hour", "REAL", false, false),
                    ("max_charger_power_kw", "REAL", false, false),
                    ("outside_temp_avg", "REAL", false, false),
                    ("start_rated_range_km", "REAL", false, false),
                    ("end_rated_range_km", "REAL", false, false),
                ],
            ),
            (
                "positions",
                true,
                &[
                    ("id", "INTEGER", true, true),
                    ("drive_id", "INTEGER", true, false),
                    ("car_id", "INTEGER", true, false),
                    ("date_ms", "INTEGER", true, false),
                    ("latitude", "REAL", true, false),
                    ("longitude", "REAL", true, false),
                    ("speed", "INTEGER", false, false),
                    ("power", "INTEGER", false, false),
                    ("battery_level", "INTEGER", false, false),
                    ("usable_battery_level", "INTEGER", false, false),
                    ("elevation", "INTEGER", false, false),
                    ("odometer", "REAL", false, false),
                    ("ideal_battery_range_km", "REAL", false, false),
                    ("rated_battery_range_km", "REAL", false, false),
                    ("is_climate_on", "INTEGER", false, false),
                    ("inside_temp", "REAL", false, false),
                    ("outside_temp", "REAL", false, false),
                ],
            ),
            (
                "charge_samples",
                true,
                &[
                    ("id", "INTEGER", true, true),
                    ("charge_process_id", "INTEGER", true, false),
                    ("timestamp_ms", "INTEGER", true, false),
                    ("battery_level", "INTEGER", false, false),
                    ("usable_battery_level", "INTEGER", false, false),
                    ("charge_energy_added_kwh", "REAL", false, false),
                    ("charger_power_kw", "REAL", false, false),
                    ("charger_voltage", "REAL", false, false),
                    ("charger_actual_current", "REAL", false, false),
                    ("charger_pilot_current", "REAL", false, false),
                    ("charger_phases", "INTEGER", false, false),
                    ("ideal_range_km", "REAL", false, false),
                    ("rated_range_km", "REAL", false, false),
                    ("outside_temp_c", "REAL", false, false),
                    ("battery_heater_on", "INTEGER", false, false),
                    ("battery_heater", "INTEGER", false, false),
                    ("battery_heater_no_power", "INTEGER", false, false),
                    ("not_enough_power_to_heat", "INTEGER", false, false),
                    ("fast_charger_present", "INTEGER", false, false),
                    ("fast_charger_brand", "TEXT", false, false),
                    ("fast_charger_type", "TEXT", false, false),
                    ("charge_cable", "TEXT", false, false),
                ],
            ),
        ];
        for (table, without_rowid, expected_columns) in expected {
            let (_, actual_without_rowid, strict) = table_flags
                .iter()
                .find(|(name, _, _)| name == table)
                .unwrap_or_else(|| panic!("missing {table} table"));
            assert_eq!(*actual_without_rowid, i64::from(*without_rowid), "{table}");
            assert_eq!(*strict, 1, "{table}");

            let mut column_statement = connection
                .prepare(&format!("PRAGMA table_xinfo('{table}')"))
                .unwrap();
            let actual_columns = column_statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            let expected_columns = expected_columns
                .iter()
                .map(|(name, declared_type, not_null, primary_key)| {
                    (
                        (*name).to_owned(),
                        (*declared_type).to_owned(),
                        i64::from(*not_null),
                        i64::from(*primary_key),
                        0,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(actual_columns, expected_columns, "{table}");
        }
    }

    #[test]
    fn writes_a_checked_typed_projection_pack() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let source = snapshot();
        let built = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot(&request(&source))
            .unwrap();
        assert_eq!(built.metadata.schema, HUB_PROJECTION_SCHEMA_V1);
        assert_eq!(built.metadata.format, PackFormat::HubProjectionSqlite);
        assert_eq!(built.metadata.row_count, 5);
        assert_eq!(
            fs::metadata(&built.path).unwrap().permissions().mode() & 0o777,
            SHARED_IMMUTABLE_PACK_MODE,
            "a collector-created immutable pack remains readable by the API data group"
        );
        assert!(
            fs::read_dir(temporary.path().join("packs/.staging"))
                .unwrap()
                .next()
                .is_none(),
            "completed publication removes its now-group-readable staging alias"
        );
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
    fn compression_uses_bounded_parallel_workers_and_is_deterministic() {
        assert_eq!(compression_worker_count_for(0), 1);
        assert_eq!(compression_worker_count_for(1), 1);
        assert_eq!(compression_worker_count_for(2), 2);
        assert_eq!(
            compression_worker_count_for(64),
            MAX_COMPRESSION_WORKERS as u32
        );

        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source.bin");
        let first = temporary.path().join("first.zst");
        let second = temporary.path().join("second.zst");
        let mut input = Vec::with_capacity(2 * 1024 * 1024);
        for index in 0..(2 * 1024 * 1024) {
            input.push((index as u8).wrapping_mul(31));
        }
        fs::write(&source, input).unwrap();
        File::create(&first).unwrap();
        File::create(&second).unwrap();

        let first_digest = compress_file_with_workers(&source, &first, 2).unwrap();
        let second_digest = compress_file_with_workers(&source, &second, 2).unwrap();
        assert_eq!(first_digest, second_digest);
        assert_eq!(fs::read(first).unwrap(), fs::read(second).unwrap());
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

        fs::set_permissions(
            temporary.path().join("packs"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
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
        let transport_rows = first
            .metadata
            .row_count
            .checked_add(second.metadata.row_count)
            .unwrap();
        let manifest = signed_full_snapshot_manifest(
            &first_request.binding,
            first_request.snapshot_id,
            first_request.sequence,
            &[first.clone(), second.clone()],
            transport_rows,
            &key,
        )
        .unwrap();
        assert_eq!(manifest.chunk_count, 2);
        assert_eq!(manifest.chunks[0].ordinal, 0);
        assert_eq!(manifest.chunks[1].ordinal, 1);
        assert_eq!(manifest.total_rows, transport_rows);
        manifest.validate_terminal_cursor(&key).unwrap();

        assert!(matches!(
            signed_full_snapshot_manifest(
                &first_request.binding,
                first_request.snapshot_id,
                first_request.sequence,
                &[first, second],
                first_request.snapshot.row_count().unwrap(),
                &key,
            ),
            Err(ProjectionPackError::Invalid(_))
        ));
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
    fn schema_2_0_rejects_positions_outside_the_legacy_layout() {
        let temporary = tempfile::tempdir().unwrap();
        let mut standalone = snapshot();
        standalone.positions[0].drive_id = None;
        assert!(matches!(
            ProjectionPackWriter::new(temporary.path().join("standalone"))
                .write_full_snapshot(&request(&standalone)),
            Err(ProjectionPackError::Invalid(message))
                if message == "schema 2.0 position.drive_id must be present"
        ));

        let mut fractional_power = snapshot();
        fractional_power.positions[0].power = Some(3.5);
        assert!(matches!(
            ProjectionPackWriter::new(temporary.path().join("fractional"))
                .write_full_snapshot(&request(&fractional_power)),
            Err(ProjectionPackError::Invalid(message))
                if message == "schema 2.0 position.power must be an integer"
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
        let setting_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM car_settings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(setting_count, 1);
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
    fn schema_2_2_full_snapshot_has_exact_physical_rows_and_full_only_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let source = snapshot_v2_2();
        let request = request_v2_2(&source);
        let built = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot_2_2(&request)
            .expect("locally validate schema 2.2 full pack");
        assert_eq!(built.metadata.schema, HUB_PROJECTION_SCHEMA_V3);
        assert_eq!(built.metadata.row_count, 13);
        built
            .metadata
            .verify_reader(File::open(&built.path).unwrap(), ProtocolLimits::default())
            .expect("schema 2.2 SQLite header matches the protocol identity");

        let sqlite = zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap();
        let inspect = temporary.path().join("inspect-2-2.sqlite");
        fs::write(&inspect, sqlite).unwrap();
        let connection = Connection::open(inspect).unwrap();
        let application_id: u32 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .unwrap();
        let user_version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(application_id, SQLITE_HUB_PROJECTION_APPLICATION_ID);
        assert_eq!(user_version, HUB_PROJECTION_SCHEMA_V3.sqlite_user_version());
        let cars: Vec<(
            i64,
            i64,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<Vec<u8>>,
            i64,
            i64,
            i64,
        )> = connection
            .prepare(
                "SELECT id, eid, vid, vin, name, model, efficiency, display_priority,
                        inserted_at_pg_us, settings_id
                 FROM cars ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            cars,
            vec![(
                10,
                101,
                201,
                Some("5YJTESTVIN1234567".into()),
                Some("Road car".into()),
                Some("Model 3".into()),
                Some((-0.145_f64).to_bits().to_be_bytes().to_vec()),
                i64::from(i16::MIN),
                1_700_000_000_000_000,
                500,
            ),]
        );
        let car_settings: (i64, i64, i64, i64, i64, i64, i64, i64) = connection
            .query_row(
                "SELECT id, suspend_min, suspend_after_idle_min, req_not_unlocked,
                        free_supercharging, use_streaming_api, enabled, lfp_battery
                 FROM car_settings WHERE id = 500",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            car_settings,
            (500, i64::from(i32::MIN), i64::from(i32::MAX), 1, 0, 1, 1, 0)
        );
        let global_settings: (
            i64,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            i64,
            i64,
        ) = connection
            .query_row(
                "SELECT id, unit_of_length, unit_of_temperature, unit_of_pressure,
                        preferred_range, base_url, grafana_url, language, theme_mode,
                        inserted_at_pg_us, updated_at_pg_us
                 FROM global_settings",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            global_settings,
            (
                i64::MIN,
                "km".into(),
                "C".into(),
                "bar".into(),
                "rated".into(),
                Some("https://teslamate.example".into()),
                None,
                String::new(),
                "system".into(),
                i64::MIN,
                i64::MAX,
            )
        );
        let state: (i64, i64, String, i64, Option<i64>) = connection
            .query_row(
                "SELECT id, car_id, state, start_date_pg_us, end_date_pg_us FROM states",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            state,
            (i64::from(i32::MIN), 10, "online".into(), i64::MIN, None)
        );
        let update: (i64, i64, i64, Option<i64>, Option<String>) = connection
            .query_row(
                "SELECT id, car_id, start_date_pg_us, end_date_pg_us, version FROM updates",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            update,
            (
                i64::from(i32::MAX),
                10,
                i64::MAX,
                Some(i64::MIN),
                Some("2026.3".into()),
            )
        );
        connection
            .execute_batch("PRAGMA foreign_keys = ON")
            .unwrap();
        for statement in [
            "UPDATE cars SET id = 32768 WHERE id = 10",
            "UPDATE cars SET display_priority = -32769 WHERE id = 10",
            "UPDATE cars SET inserted_at_pg_us = 1 WHERE id = 10",
            "UPDATE cars SET updated_at_pg_us = -9223372036854775807 WHERE id = 10",
            "UPDATE cars SET model = replace(hex(zeroblob(256)), '00', 'x') WHERE id = 10",
            "UPDATE cars SET efficiency = x'00000000000000' WHERE id = 10",
            "UPDATE cars SET settings_id = 999 WHERE id = 10",
            "UPDATE car_settings SET suspend_min = 2147483648 WHERE id = 500",
            "UPDATE global_settings SET unit_of_length = 'kilometres'",
            "UPDATE global_settings SET unit_of_temperature = 'K'",
            "UPDATE global_settings SET unit_of_pressure = 'kpa'",
            "UPDATE global_settings SET preferred_range = 'full'",
            "UPDATE global_settings SET base_url = replace(hex(zeroblob(256)), '00', 'x')",
            "UPDATE global_settings SET language = replace(hex(zeroblob(16385)), '00', 'x')",
            "UPDATE global_settings SET inserted_at_pg_us = 1",
            "UPDATE global_settings SET updated_at_pg_us = -9223372036854775807",
            "UPDATE states SET id = 2147483648 WHERE id = -2147483648",
            "UPDATE states SET car_id = 32768 WHERE id = -2147483648",
            "UPDATE states SET state = 'driving' WHERE id = -2147483648",
            "UPDATE states SET start_date_pg_us = -9223372036854775807 WHERE id = -2147483648",
            "UPDATE states SET start_date_pg_us = -211813488000000001 WHERE id = -2147483648",
            "UPDATE states SET start_date_pg_us = 9223371331200000000 WHERE id = -2147483648",
            "UPDATE updates SET id = -2147483649 WHERE id = 2147483647",
            "UPDATE updates SET car_id = -32769 WHERE id = 2147483647",
            "UPDATE updates SET end_date_pg_us = 9223372036854775806 WHERE id = 2147483647",
            "UPDATE updates SET version = replace(hex(zeroblob(256)), '00', 'x') WHERE id = 2147483647",
        ] {
            assert!(connection.execute(statement, []).is_err(), "{statement}");
        }
        connection
            .execute(
                "UPDATE states SET start_date_pg_us = -211813488000000000, end_date_pg_us = 9223371331199999999 WHERE id = -2147483648",
                [],
            )
            .expect("finite PostgreSQL timestamp bounds are physical source values");
        connection
            .execute(
                "UPDATE updates SET start_date_pg_us = (-9223372036854775807 - 1), end_date_pg_us = 9223372036854775807 WHERE id = 2147483647",
                [],
            )
            .expect("PostgreSQL infinity timestamp sentinels are physical source values");
        connection
            .execute(
                "UPDATE updates SET end_date_pg_us = NULL, version = '' WHERE id = 2147483647",
                [],
            )
            .expect("nullable update end/version and an empty varchar are physical source values");
        let address_columns: Vec<String> = connection
            .prepare("PRAGMA table_xinfo('addresses')")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            address_columns,
            vec![
                "id",
                "display_name",
                "latitude_e6",
                "latitude_e6_is_nan",
                "longitude_e6",
                "longitude_e6_is_nan",
                "name",
                "house_number",
                "road",
                "neighbourhood",
                "city",
                "county",
                "postcode",
                "state",
                "state_district",
                "country",
                "inserted_at_pg_us",
                "updated_at_pg_us",
                "osm_id",
                "osm_type",
            ]
        );
        assert!(
            !address_columns.contains(&"raw".to_owned()),
            "the sensitive source payload has no schema/output representation"
        );
        let metadata: Vec<(String, String)> = connection
            .prepare(
                "SELECT key, value FROM hub_pack_metadata
                 WHERE key IN (
                   'ledger_state', 'ledger_slice', 'mapped_fields', 'unreconciled_fields',
                   'source_revision', 'migration_set_sha256', 'car_settings_slice_sha256',
                   'settings_slice_sha256',
                   'cars_efficiency_encoding', 'cars_slice_sha256', 'address_slice_sha256',
                   'drives_float_encoding', 'drives_slice_sha256', 'fixed_numeric_encoding',
                   'geofence_slice_sha256', 'positions_odometer_encoding',
                   'positions_relation_scope', 'positions_slice_sha256',
                   'charging_boolean_encoding', 'charges_relation_scope',
                   'charging_processes_slice_sha256', 'charges_slice_sha256',
                   'postgres_timestamp_encoding',
                   'postgres_timestamp_0_encoding',
                   'states_slice_sha256', 'updates_slice_sha256',
                   'reconciliation', 'schema_support', 'publication_scope'
                 )
                 ORDER BY key",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut expected_metadata = vec![
                ("address_slice_sha256".into(), thp2_2_address_slice_sha256()),
                (
                    "car_settings_slice_sha256".into(),
                    thp2_2_car_settings_slice_sha256()
                ),
                (
                    "settings_slice_sha256".into(),
                    thp2_2_global_settings_slice_sha256()
                ),
                (
                    "cars_efficiency_encoding".into(),
                    THP2_2_CARS_EFFICIENCY_ENCODING.into()
                ),
                ("cars_slice_sha256".into(), thp2_2_cars_slice_sha256()),
                (
                    "charging_boolean_encoding".into(),
                    THP2_2_CHARGING_TRI_STATE_BOOL_ENCODING.into()
                ),
                (
                    "charges_relation_scope".into(),
                    THP2_2_CHARGES_RELATION_SCOPE.into()
                ),
                (
                    "charging_processes_slice_sha256".into(),
                    thp2_2_charging_processes_slice_sha256()
                ),
                ("charges_slice_sha256".into(), thp2_2_charges_slice_sha256()),
                (
                    "drives_float_encoding".into(),
                    THP2_2_DRIVES_FLOAT_ENCODING.into()
                ),
                ("drives_slice_sha256".into(), thp2_2_drives_slice_sha256()),
                (
                    "fixed_numeric_encoding".into(),
                    THP2_2_FIXED_NUMERIC_ENCODING.into()
                ),
                (
                    "geofence_slice_sha256".into(),
                    thp2_2_geofence_slice_sha256()
                ),
                (
                    "ledger_slice".into(),
                    "settings+car_settings+cars+drives+positions+charging_processes+charges+addresses+geofences+states+updates".into()
                ),
                ("ledger_state".into(), "draft_blocked".into()),
                ("mapped_fields".into(), "168".into()),
                (
                    "migration_set_sha256".into(),
                    TESLAMATE_V4_MIGRATION_SET_SHA256.into(),
                ),
                (
                    "positions_odometer_encoding".into(),
                    THP2_2_POSITIONS_ODOMETER_ENCODING.into()
                ),
                (
                    "positions_relation_scope".into(),
                    THP2_2_POSITIONS_RELATION_SCOPE.into()
                ),
                (
                    "positions_slice_sha256".into(),
                    thp2_2_positions_slice_sha256()
                ),
                (
                    "postgres_timestamp_0_encoding".into(),
                    THP2_2_POSTGRES_TIMESTAMP_0_ENCODING.into()
                ),
                (
                    "postgres_timestamp_encoding".into(),
                    THP2_2_POSTGRES_TIMESTAMP_ENCODING.into()
                ),
                ("publication_scope".into(), "local_validation_only".into()),
                ("reconciliation".into(), "not_run".into()),
                ("schema_support".into(), "full_snapshot_only".into()),
                (
                    "source_revision".into(),
                    TESLAMATE_V4_SOURCE_REVISION.into()
                ),
                ("states_slice_sha256".into(), thp2_2_states_slice_sha256()),
                ("unreconciled_fields".into(), "1".into()),
                ("updates_slice_sha256".into(), thp2_2_updates_slice_sha256()),
            ];
        expected_metadata.sort_unstable();
        assert_eq!(metadata, expected_metadata);
        #[derive(Debug, PartialEq)]
        struct DriveRow {
            id: i64,
            car_id: i64,
            start_date_pg_us: i64,
            end_date_pg_us: Option<i64>,
            start_position_id: Option<i64>,
            end_position_id: Option<i64>,
            start_address_id: Option<i64>,
            end_address_id: Option<i64>,
            start_geofence_id: Option<i64>,
            end_geofence_id: Option<i64>,
            outside_temp_avg_e1: Option<i64>,
            outside_temp_avg_e1_is_nan: i64,
            inside_temp_avg_e1: Option<i64>,
            inside_temp_avg_e1_is_nan: i64,
            speed_max: Option<i64>,
            power_max: Option<i64>,
            power_min: Option<i64>,
            start_ideal_range_km_e2: Option<i64>,
            start_ideal_range_km_e2_is_nan: i64,
            end_ideal_range_km_e2: Option<i64>,
            end_ideal_range_km_e2_is_nan: i64,
            start_rated_range_km_e2: Option<i64>,
            start_rated_range_km_e2_is_nan: i64,
            end_rated_range_km_e2: Option<i64>,
            end_rated_range_km_e2_is_nan: i64,
            start_km_f64_be: Option<Vec<u8>>,
            end_km_f64_be: Option<Vec<u8>>,
            distance_f64_be: Option<Vec<u8>>,
            duration_min: Option<i64>,
            ascent: Option<i64>,
            descent: Option<i64>,
        }
        let drive = connection
            .query_row(
                "SELECT id, car_id, start_date_pg_us, end_date_pg_us, start_position_id,
                        end_position_id, start_address_id, end_address_id, start_geofence_id,
                        end_geofence_id, outside_temp_avg_e1, outside_temp_avg_e1_is_nan,
                        inside_temp_avg_e1, inside_temp_avg_e1_is_nan, speed_max, power_max,
                        power_min, start_ideal_range_km_e2, start_ideal_range_km_e2_is_nan,
                        end_ideal_range_km_e2, end_ideal_range_km_e2_is_nan,
                        start_rated_range_km_e2, start_rated_range_km_e2_is_nan,
                        end_rated_range_km_e2, end_rated_range_km_e2_is_nan, start_km_f64_be,
                        end_km_f64_be, distance_f64_be, duration_min, ascent, descent
                 FROM drives WHERE id = 20",
                [],
                |row| {
                    Ok(DriveRow {
                        id: row.get(0)?,
                        car_id: row.get(1)?,
                        start_date_pg_us: row.get(2)?,
                        end_date_pg_us: row.get(3)?,
                        start_position_id: row.get(4)?,
                        end_position_id: row.get(5)?,
                        start_address_id: row.get(6)?,
                        end_address_id: row.get(7)?,
                        start_geofence_id: row.get(8)?,
                        end_geofence_id: row.get(9)?,
                        outside_temp_avg_e1: row.get(10)?,
                        outside_temp_avg_e1_is_nan: row.get(11)?,
                        inside_temp_avg_e1: row.get(12)?,
                        inside_temp_avg_e1_is_nan: row.get(13)?,
                        speed_max: row.get(14)?,
                        power_max: row.get(15)?,
                        power_min: row.get(16)?,
                        start_ideal_range_km_e2: row.get(17)?,
                        start_ideal_range_km_e2_is_nan: row.get(18)?,
                        end_ideal_range_km_e2: row.get(19)?,
                        end_ideal_range_km_e2_is_nan: row.get(20)?,
                        start_rated_range_km_e2: row.get(21)?,
                        start_rated_range_km_e2_is_nan: row.get(22)?,
                        end_rated_range_km_e2: row.get(23)?,
                        end_rated_range_km_e2_is_nan: row.get(24)?,
                        start_km_f64_be: row.get(25)?,
                        end_km_f64_be: row.get(26)?,
                        distance_f64_be: row.get(27)?,
                        duration_min: row.get(28)?,
                        ascent: row.get(29)?,
                        descent: row.get(30)?,
                    })
                },
            )
            .unwrap();
        assert_eq!(
            drive,
            DriveRow {
                id: 20,
                car_id: 10,
                start_date_pg_us: i64::MAX,
                end_date_pg_us: Some(i64::MIN),
                start_position_id: Some(i64::from(i32::MIN)),
                end_position_id: Some(i64::from(i32::MAX)),
                start_address_id: Some(100),
                end_address_id: Some(101),
                start_geofence_id: Some(200),
                end_geofence_id: Some(201),
                outside_temp_avg_e1: None,
                outside_temp_avg_e1_is_nan: 1,
                inside_temp_avg_e1: Some(-9_999),
                inside_temp_avg_e1_is_nan: 0,
                speed_max: Some(i64::from(i16::MIN)),
                power_max: Some(i64::from(i16::MAX)),
                power_min: Some(i64::from(i16::MIN)),
                start_ideal_range_km_e2: Some(999_999),
                start_ideal_range_km_e2_is_nan: 0,
                end_ideal_range_km_e2: Some(-999_999),
                end_ideal_range_km_e2_is_nan: 0,
                start_rated_range_km_e2: None,
                start_rated_range_km_e2_is_nan: 1,
                end_rated_range_km_e2: None,
                end_rated_range_km_e2_is_nan: 0,
                start_km_f64_be: Some((-0.0_f64).to_bits().to_be_bytes().to_vec()),
                end_km_f64_be: Some(0x7ff8_0000_0000_0042_u64.to_be_bytes().to_vec()),
                distance_f64_be: Some(f64::INFINITY.to_bits().to_be_bytes().to_vec()),
                duration_min: Some(i64::from(i16::MIN)),
                ascent: Some(i64::from(i16::MAX)),
                descent: Some(i64::from(i16::MIN)),
            }
        );
        let quoted_row = |query: &str| {
            connection
                .query_row(query, [], |row| {
                    (0..row.as_ref().column_count())
                        .map(|index| row.get(index))
                        .collect::<rusqlite::Result<Vec<String>>>()
                })
                .unwrap()
        };
        assert_eq!(
            quoted_row(
                "SELECT quote(id), quote(car_id), quote(position_id), quote(address_id),
                        quote(geofence_id), quote(start_date_pg_us), quote(end_date_pg_us),
                        quote(charge_energy_added_e2), quote(charge_energy_added_e2_is_nan),
                        quote(charge_energy_used_e2), quote(charge_energy_used_e2_is_nan),
                        quote(start_ideal_range_km_e2), quote(start_ideal_range_km_e2_is_nan),
                        quote(end_ideal_range_km_e2), quote(end_ideal_range_km_e2_is_nan),
                        quote(start_rated_range_km_e2), quote(start_rated_range_km_e2_is_nan),
                        quote(end_rated_range_km_e2), quote(end_rated_range_km_e2_is_nan),
                        quote(start_battery_level), quote(end_battery_level), quote(duration_min),
                        quote(outside_temp_avg_e1), quote(outside_temp_avg_e1_is_nan),
                        quote(cost_e2), quote(cost_e2_is_nan)
                 FROM charging_processes WHERE id = 40",
            ),
            vec![
                "40",
                "10",
                "30",
                "100",
                "200",
                "-9223372036854775808",
                "9223372036854775807",
                "NULL",
                "1",
                "NULL",
                "0",
                "999999",
                "0",
                "-999999",
                "0",
                "NULL",
                "1",
                "NULL",
                "0",
                "-32768",
                "32767",
                "-32768",
                "-9999",
                "0",
                "999999",
                "0",
            ]
        );
        assert_eq!(
            quoted_row(
                "SELECT quote(id), quote(charging_process_id), quote(date_pg_us),
                        quote(battery_heater), quote(battery_heater_on),
                        quote(battery_heater_no_power), quote(battery_level),
                        quote(usable_battery_level), quote(charge_energy_added_e2),
                        quote(charge_energy_added_e2_is_nan), quote(charger_actual_current),
                        quote(charger_phases), quote(charger_pilot_current), quote(charger_power),
                        quote(charger_voltage), quote(conn_charge_cable),
                        quote(fast_charger_present), quote(fast_charger_brand),
                        quote(fast_charger_type), quote(ideal_battery_range_km_e2),
                        quote(ideal_battery_range_km_e2_is_nan), quote(rated_battery_range_km_e2),
                        quote(rated_battery_range_km_e2_is_nan), quote(not_enough_power_to_heat),
                        quote(outside_temp_e1), quote(outside_temp_e1_is_nan)
                 FROM charges WHERE id = 50",
            ),
            vec![
                "50",
                "40",
                "9223372036854775807",
                "0",
                "1",
                "NULL",
                "-32768",
                "32767",
                "NULL",
                "1",
                "-32768",
                "32767",
                "-32768",
                "32767",
                "-32768",
                "'Type 2'",
                "0",
                "'Tesla'",
                "'Supercharger'",
                "-999999",
                "0",
                "NULL",
                "1",
                "0",
                "9999",
                "0",
            ]
        );
        for statement in [
            "UPDATE drives SET id = 2147483648 WHERE id = 20",
            "UPDATE drives SET car_id = 32768 WHERE id = 20",
            "UPDATE drives SET start_date_pg_us = -9223372036854775807 WHERE id = 20",
            "UPDATE drives SET end_date_pg_us = 9223371331200000000 WHERE id = 20",
            "UPDATE drives SET start_position_id = 2147483648 WHERE id = 20",
            "UPDATE drives SET outside_temp_avg_e1 = 10000, outside_temp_avg_e1_is_nan = 0 WHERE id = 20",
            "UPDATE drives SET inside_temp_avg_e1 = 1, inside_temp_avg_e1_is_nan = 1 WHERE id = 20",
            "UPDATE drives SET start_ideal_range_km_e2_is_nan = 2 WHERE id = 20",
            "UPDATE drives SET start_km_f64_be = x'00000000000000' WHERE id = 20",
            "UPDATE drives SET duration_min = 32768 WHERE id = 20",
        ] {
            assert!(
                connection.execute(statement, []).is_err(),
                "{statement} must violate the exact physical drives DDL"
            );
        }
        connection
            .execute(
                "UPDATE drives SET end_date_pg_us = NULL, start_position_id = -2147483648,
                        end_position_id = 2147483647, start_address_id = -2147483648,
                        end_address_id = 2147483647, start_geofence_id = -2147483648,
                        end_geofence_id = 2147483647 WHERE id = 20",
                [],
            )
            .expect("open and signed raw drive values have no invented semantic policy");
        let charging_process_refs: (Option<i64>, Option<i64>) = connection
            .query_row(
                "SELECT address_id, geofence_id FROM charging_processes WHERE id = 40",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(charging_process_refs, (Some(100), Some(200)));
        #[derive(Debug, PartialEq)]
        struct AddressRow {
            id: i64,
            display_name: Option<String>,
            latitude_e6: Option<i64>,
            latitude_e6_is_nan: i64,
            longitude_e6: Option<i64>,
            longitude_e6_is_nan: i64,
            name: Option<String>,
            house_number: Option<String>,
            road: Option<String>,
            neighbourhood: Option<String>,
            city: Option<String>,
            county: Option<String>,
            postcode: Option<String>,
            state: Option<String>,
            state_district: Option<String>,
            country: Option<String>,
            inserted_at_pg_us: i64,
            updated_at_pg_us: i64,
            osm_id: Option<i64>,
            osm_type: Option<String>,
        }
        let addresses: Vec<AddressRow> = connection
            .prepare(
                "SELECT id, display_name, latitude_e6, latitude_e6_is_nan, longitude_e6,
                        longitude_e6_is_nan, name, house_number, road,
                        neighbourhood, city, county, postcode, state, state_district, country,
                        inserted_at_pg_us, updated_at_pg_us, osm_id, osm_type
                 FROM addresses ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok(AddressRow {
                    id: row.get(0)?,
                    display_name: row.get(1)?,
                    latitude_e6: row.get(2)?,
                    latitude_e6_is_nan: row.get(3)?,
                    longitude_e6: row.get(4)?,
                    longitude_e6_is_nan: row.get(5)?,
                    name: row.get(6)?,
                    house_number: row.get(7)?,
                    road: row.get(8)?,
                    neighbourhood: row.get(9)?,
                    city: row.get(10)?,
                    county: row.get(11)?,
                    postcode: row.get(12)?,
                    state: row.get(13)?,
                    state_district: row.get(14)?,
                    country: row.get(15)?,
                    inserted_at_pg_us: row.get(16)?,
                    updated_at_pg_us: row.get(17)?,
                    osm_id: row.get(18)?,
                    osm_type: row.get(19)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            addresses,
            vec![
                AddressRow {
                    id: 100,
                    display_name: Some("Home, London".into()),
                    latitude_e6: Some(51_500_123),
                    latitude_e6_is_nan: 0,
                    longitude_e6: Some(-123_456),
                    longitude_e6_is_nan: 0,
                    name: Some("Home".into()),
                    house_number: Some("1".into()),
                    road: Some("Strawberry Road".into()),
                    neighbourhood: Some("Westminster".into()),
                    city: Some("London".into()),
                    county: Some("Greater London".into()),
                    postcode: Some("SW1A 1AA".into()),
                    state: Some("England".into()),
                    state_district: Some("London".into()),
                    country: Some("United Kingdom".into()),
                    inserted_at_pg_us: 1_700_000_000_000_000,
                    updated_at_pg_us: 1_700_000_100_000_000,
                    osm_id: Some(-42),
                    osm_type: Some("node".into()),
                },
                AddressRow {
                    id: 101,
                    display_name: Some("Work, London".into()),
                    latitude_e6: None,
                    latitude_e6_is_nan: 1,
                    longitude_e6: None,
                    longitude_e6_is_nan: 0,
                    name: Some("Work".into()),
                    house_number: None,
                    road: None,
                    neighbourhood: None,
                    city: None,
                    county: None,
                    postcode: None,
                    state: None,
                    state_district: None,
                    country: None,
                    inserted_at_pg_us: 1_700_000_200_000_000,
                    updated_at_pg_us: 1_700_000_300_000_000,
                    osm_id: None,
                    osm_type: None,
                },
            ]
        );
        assert!(
            connection
                .execute(
                    "UPDATE addresses SET latitude_e6 = 100000000 WHERE id = 100",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE addresses SET longitude_e6 = -1000000000 WHERE id = 100",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE addresses SET latitude_e6 = 1, latitude_e6_is_nan = 1 WHERE id = 100",
                    [],
                )
                .is_err(),
            "finite numeric and NaN tag must not be conflated"
        );
        assert!(
            connection
                .execute(
                    "UPDATE addresses SET longitude_e6_is_nan = 2 WHERE id = 100",
                    [],
                )
                .is_err(),
            "numeric NaN tag must be binary"
        );
        assert!(
            connection
                .execute("UPDATE addresses SET id = 2147483648 WHERE id = 100", [],)
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE addresses SET inserted_at_pg_us = 1 WHERE id = 100",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE addresses SET updated_at_pg_us = -9223372036854775807 WHERE id = 100",
                    [],
                )
                .is_err()
        );
        #[derive(Debug, PartialEq)]
        struct GeofenceRow {
            id: i64,
            name: String,
            latitude_e6: Option<i64>,
            latitude_e6_is_nan: i64,
            longitude_e6: Option<i64>,
            longitude_e6_is_nan: i64,
            radius: i64,
            billing_type: String,
            cost_per_unit_e4: Option<i64>,
            cost_per_unit_e4_is_nan: i64,
            session_fee_e2: Option<i64>,
            session_fee_e2_is_nan: i64,
            inserted_at_pg_us: i64,
            updated_at_pg_us: i64,
        }
        let geofences: Vec<GeofenceRow> = connection
            .prepare(
                "SELECT id, name, latitude_e6, latitude_e6_is_nan, longitude_e6,
                        longitude_e6_is_nan, radius, billing_type, cost_per_unit_e4,
                        cost_per_unit_e4_is_nan, session_fee_e2, session_fee_e2_is_nan,
                        inserted_at_pg_us, updated_at_pg_us
                 FROM geofences ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok(GeofenceRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    latitude_e6: row.get(2)?,
                    latitude_e6_is_nan: row.get(3)?,
                    longitude_e6: row.get(4)?,
                    longitude_e6_is_nan: row.get(5)?,
                    radius: row.get(6)?,
                    billing_type: row.get(7)?,
                    cost_per_unit_e4: row.get(8)?,
                    cost_per_unit_e4_is_nan: row.get(9)?,
                    session_fee_e2: row.get(10)?,
                    session_fee_e2_is_nan: row.get(11)?,
                    inserted_at_pg_us: row.get(12)?,
                    updated_at_pg_us: row.get(13)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            geofences,
            vec![
                GeofenceRow {
                    id: 200,
                    name: "Home".into(),
                    latitude_e6: Some(0),
                    latitude_e6_is_nan: 0,
                    longitude_e6: None,
                    longitude_e6_is_nan: 1,
                    radius: i64::from(i16::MIN),
                    billing_type: "per_kwh".into(),
                    cost_per_unit_e4: Some(3_000),
                    cost_per_unit_e4_is_nan: 0,
                    session_fee_e2: None,
                    session_fee_e2_is_nan: 1,
                    inserted_at_pg_us: 1_700_000_000_000_000,
                    updated_at_pg_us: 1_700_000_100_000_000,
                },
                GeofenceRow {
                    id: 201,
                    name: "Work".into(),
                    latitude_e6: None,
                    latitude_e6_is_nan: 1,
                    longitude_e6: Some(-110_000),
                    longitude_e6_is_nan: 0,
                    radius: i64::from(i16::MAX),
                    billing_type: "per_minute".into(),
                    cost_per_unit_e4: None,
                    cost_per_unit_e4_is_nan: 0,
                    session_fee_e2: None,
                    session_fee_e2_is_nan: 0,
                    inserted_at_pg_us: 1_700_000_200_000_000,
                    updated_at_pg_us: 1_700_000_300_000_000,
                },
            ]
        );
        assert!(
            connection
                .execute(
                    "UPDATE geofences SET latitude_e6 = NULL, latitude_e6_is_nan = 0 WHERE id = 200",
                    [],
                )
                .is_err(),
            "required source numeric must be finite or NaN"
        );
        assert!(
            connection
                .execute(
                    "UPDATE geofences SET cost_per_unit_e4 = 1, cost_per_unit_e4_is_nan = 1 WHERE id = 200",
                    [],
                )
                .is_err(),
            "optional source numeric finite and NaN states must stay distinct"
        );
        for statement in [
            "UPDATE geofences SET id = 2147483648 WHERE id = 200",
            "UPDATE geofences SET name = replace(hex(zeroblob(256)), '00', 'x') WHERE id = 200",
            "UPDATE geofences SET latitude_e6 = 100000000 WHERE id = 200",
            "UPDATE geofences SET cost_per_unit_e4 = 1000000 WHERE id = 200",
            "UPDATE geofences SET inserted_at_pg_us = 1 WHERE id = 200",
            "UPDATE geofences SET updated_at_pg_us = -9223372036854775807 WHERE id = 200",
        ] {
            assert!(
                connection.execute(statement, []).is_err(),
                "{statement} must violate the physical geofence DDL"
            );
        }
        let settings_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM car_settings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(settings_rows, 1);
        let geofence_columns: Vec<String> = connection
            .prepare("PRAGMA table_xinfo('geofences')")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for lossy_column in [
            "latitude",
            "longitude",
            "radius_m",
            "cost_per_unit",
            "session_fee",
        ] {
            assert!(
                !geofence_columns.contains(&lossy_column.to_owned()),
                "schema 2.2 geofence must retain exact fixed-scale source values"
            );
        }
        let foreign_key_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(foreign_key_rows, 0);
        let drive_columns: Vec<String> = connection
            .prepare("PRAGMA table_xinfo('drives')")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!drive_columns.contains(&"start_address".into()));
        assert!(!drive_columns.contains(&"end_address".into()));
        assert!(!drive_columns.contains(&"start_geofence".into()));
        assert!(!drive_columns.contains(&"end_geofence".into()));
    }

    #[test]
    fn schema_2_2_address_contract_hash_and_ddl_are_pinned() {
        assert_eq!(
            Sha256Digest::of_bytes(THP2_2_ADDRESS_SLICE_CONTRACT.as_bytes()).to_string(),
            THP2_2_ADDRESS_SLICE_SHA256
        );

        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(THP2_2_ADDRESSES_SQLITE_DDL)
            .unwrap();
        verify_projection_table_ddl(&connection, "addresses", THP2_2_ADDRESSES_SQLITE_DDL)
            .expect("canonical address DDL must verify");

        let unchecked = THP2_2_ADDRESSES_SQLITE_DDL
            .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
            .replace(" CHECK(latitude_e6_is_nan IN (0, 1))", "");
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(&unchecked).unwrap();
        assert!(
            verify_projection_table_ddl(&connection, "addresses", THP2_2_ADDRESSES_SQLITE_DDL)
                .is_err(),
            "the verifier must reject an addresses table recreated without physical checks"
        );
    }

    #[test]
    fn schema_2_2_geofence_contract_hash_and_ddl_are_pinned() {
        assert_eq!(
            Sha256Digest::of_bytes(THP2_2_GEOFENCE_SLICE_CONTRACT.as_bytes()).to_string(),
            THP2_2_GEOFENCE_SLICE_SHA256
        );

        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(THP2_2_GEOFENCES_SQLITE_DDL)
            .unwrap();
        verify_projection_table_ddl(&connection, "geofences", THP2_2_GEOFENCES_SQLITE_DDL)
            .expect("canonical geofence DDL must verify");

        let unchecked = THP2_2_GEOFENCES_SQLITE_DDL
            .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
            .replace(" CHECK(latitude_e6_is_nan IN (0, 1))", "");
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(&unchecked).unwrap();
        assert!(
            verify_projection_table_ddl(&connection, "geofences", THP2_2_GEOFENCES_SQLITE_DDL)
                .is_err(),
            "the verifier must reject a geofences table recreated without tagged numeric checks"
        );
    }

    #[test]
    fn schema_2_2_global_settings_contract_hash_ddl_and_singleton_are_pinned() {
        assert_eq!(
            Sha256Digest::of_bytes(THP2_2_GLOBAL_SETTINGS_SLICE_CONTRACT.as_bytes()).to_string(),
            THP2_2_GLOBAL_SETTINGS_SLICE_SHA256
        );
        assert_eq!(
            serde_json::to_string(&ProjectionUnitOfLengthV2_2::Kilometers).unwrap(),
            "\"km\""
        );
        assert_eq!(
            serde_json::to_string(&ProjectionUnitOfLengthV2_2::Miles).unwrap(),
            "\"mi\""
        );
        let length_round_trip = serde_json::to_string(&ProjectionUnitOfLengthV2_2::Miles).unwrap();
        assert_eq!(
            serde_json::from_str::<ProjectionUnitOfLengthV2_2>(&length_round_trip).unwrap(),
            ProjectionUnitOfLengthV2_2::Miles
        );
        assert_eq!(
            serde_json::from_str::<ProjectionUnitOfLengthV2_2>("\"mi\"").unwrap(),
            ProjectionUnitOfLengthV2_2::Miles
        );
        assert!(serde_json::from_str::<ProjectionUnitOfLengthV2_2>("\"Miles\"").is_err());
        assert_eq!(
            serde_json::to_string(&ProjectionUnitOfTemperatureV2_2::Celsius).unwrap(),
            "\"C\""
        );
        assert_eq!(
            serde_json::to_string(&ProjectionUnitOfTemperatureV2_2::Fahrenheit).unwrap(),
            "\"F\""
        );
        let temperature_round_trip =
            serde_json::to_string(&ProjectionUnitOfTemperatureV2_2::Fahrenheit).unwrap();
        assert_eq!(
            serde_json::from_str::<ProjectionUnitOfTemperatureV2_2>(&temperature_round_trip)
                .unwrap(),
            ProjectionUnitOfTemperatureV2_2::Fahrenheit
        );
        assert_eq!(
            serde_json::from_str::<ProjectionUnitOfTemperatureV2_2>("\"F\"").unwrap(),
            ProjectionUnitOfTemperatureV2_2::Fahrenheit
        );
        assert!(serde_json::from_str::<ProjectionUnitOfTemperatureV2_2>("\"Kelvin\"").is_err());
        assert_eq!(
            serde_json::to_string(&ProjectionUnitOfPressureV2_2::Bar).unwrap(),
            "\"bar\""
        );
        assert_eq!(
            serde_json::to_string(&ProjectionUnitOfPressureV2_2::Psi).unwrap(),
            "\"psi\""
        );
        let pressure_round_trip =
            serde_json::to_string(&ProjectionUnitOfPressureV2_2::Psi).unwrap();
        assert_eq!(
            serde_json::from_str::<ProjectionUnitOfPressureV2_2>(&pressure_round_trip).unwrap(),
            ProjectionUnitOfPressureV2_2::Psi
        );
        assert_eq!(
            serde_json::from_str::<ProjectionUnitOfPressureV2_2>("\"psi\"").unwrap(),
            ProjectionUnitOfPressureV2_2::Psi
        );
        assert!(serde_json::from_str::<ProjectionUnitOfPressureV2_2>("\"kpa\"").is_err());
        assert_eq!(
            serde_json::to_string(&ProjectionPreferredRangeV2_2::Ideal).unwrap(),
            "\"ideal\""
        );
        assert_eq!(
            serde_json::to_string(&ProjectionPreferredRangeV2_2::Rated).unwrap(),
            "\"rated\""
        );
        let range_round_trip = serde_json::to_string(&ProjectionPreferredRangeV2_2::Rated).unwrap();
        assert_eq!(
            serde_json::from_str::<ProjectionPreferredRangeV2_2>(&range_round_trip).unwrap(),
            ProjectionPreferredRangeV2_2::Rated
        );
        assert_eq!(
            serde_json::from_str::<ProjectionPreferredRangeV2_2>("\"rated\"").unwrap(),
            ProjectionPreferredRangeV2_2::Rated
        );
        assert!(serde_json::from_str::<ProjectionPreferredRangeV2_2>("\"preferred\"").is_err());

        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(THP2_2_GLOBAL_SETTINGS_SQLITE_DDL)
            .unwrap();
        verify_projection_table_ddl(
            &connection,
            "global_settings",
            THP2_2_GLOBAL_SETTINGS_SQLITE_DDL,
        )
        .expect("canonical global settings DDL must verify");
        verify_projection_foreign_keys(&connection, "global_settings", &[])
            .expect("global source settings have no local SQLite foreign keys");

        let unchecked = THP2_2_GLOBAL_SETTINGS_SQLITE_DDL
            .replace(" CHECK(unit_of_length IN ('km', 'mi'))", "")
            .replace(" CHECK(base_url IS NULL OR length(base_url) <= 255)", "")
            .replace(
                " CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0))",
                "",
            );
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(&unchecked).unwrap();
        assert!(
            verify_projection_table_ddl(
                &connection,
                "global_settings",
                THP2_2_GLOBAL_SETTINGS_SQLITE_DDL,
            )
            .is_err(),
            "the verifier must reject global settings recreated without physical checks"
        );

        let source = snapshot_v2_2();
        assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());
        let mut missing = source.clone();
        missing.global_settings.clear();
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&missing), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "schema 2.2 physical snapshot must contain exactly one global_settings row"
        ));
        let mut duplicate = source.clone();
        duplicate
            .global_settings
            .push(source.global_settings[0].clone());
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&duplicate), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "schema 2.2 physical snapshot must contain exactly one global_settings row"
        ));
        let mut url_boundary = source.clone();
        url_boundary.global_settings[0].base_url = Some("é".repeat(255));
        assert!(
            validate_request_v2_2(&request_v2_2(&url_boundary), ProtocolLimits::default()).is_ok()
        );
        url_boundary.global_settings[0].grafana_url = Some("é".repeat(256));
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&url_boundary), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "global_settings.grafana_url exceeds its pinned source width"
        ));
        let mut unsafe_text = source;
        unsafe_text.global_settings[0].language = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&unsafe_text), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "global_settings.language is unsafe or too large"
        ));
    }

    #[test]
    fn schema_2_2_cars_and_car_settings_contract_hashes_and_ddl_are_pinned() {
        assert_eq!(
            Sha256Digest::of_bytes(THP2_2_CAR_SETTINGS_SLICE_CONTRACT.as_bytes()).to_string(),
            THP2_2_CAR_SETTINGS_SLICE_SHA256
        );
        assert_eq!(
            Sha256Digest::of_bytes(THP2_2_CARS_SLICE_CONTRACT.as_bytes()).to_string(),
            THP2_2_CARS_SLICE_SHA256
        );

        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(THP2_2_CAR_SETTINGS_SQLITE_DDL)
            .unwrap();
        connection.execute_batch(THP2_2_CARS_SQLITE_DDL).unwrap();
        verify_projection_table_ddl(&connection, "car_settings", THP2_2_CAR_SETTINGS_SQLITE_DDL)
            .expect("canonical car_settings DDL must verify");
        verify_projection_table_ddl(&connection, "cars", THP2_2_CARS_SQLITE_DDL)
            .expect("canonical cars DDL must verify");

        let unchecked_settings = THP2_2_CAR_SETTINGS_SQLITE_DDL
            .replace(" CHECK(suspend_min BETWEEN -2147483648 AND 2147483647)", "")
            .replace(" CHECK(enabled IN (0, 1))", "");
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(&unchecked_settings).unwrap();
        assert!(
            verify_projection_table_ddl(
                &connection,
                "car_settings",
                THP2_2_CAR_SETTINGS_SQLITE_DDL
            )
            .is_err(),
            "the verifier must reject a car_settings table recreated without physical checks"
        );

        let unchecked_cars = THP2_2_CARS_SQLITE_DDL
            .replace(" CHECK(id BETWEEN -32768 AND 32767)", "")
            .replace(" CHECK(model IS NULL OR length(model) <= 255)", "")
            .replace(" CHECK(efficiency IS NULL OR length(efficiency) = 8)", "")
            .replace(
                " CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0))",
                "",
            )
            .replace(
                " UNIQUE REFERENCES car_settings(id)",
                " REFERENCES car_settings(id)",
            );
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(THP2_2_CAR_SETTINGS_SQLITE_DDL)
            .unwrap();
        connection.execute_batch(&unchecked_cars).unwrap();
        assert!(
            verify_projection_table_ddl(&connection, "cars", THP2_2_CARS_SQLITE_DDL).is_err(),
            "the verifier must reject a cars table recreated without physical checks or unique FK"
        );
    }

    #[test]
    fn schema_2_2_drives_contract_hash_ddl_and_no_outgoing_foreign_keys_are_pinned() {
        assert_eq!(
            Sha256Digest::of_bytes(THP2_2_DRIVES_SLICE_CONTRACT.as_bytes()).to_string(),
            THP2_2_DRIVES_SLICE_SHA256
        );
        assert_eq!(THP2_2_DRIVES_FLOAT_ENCODING, "ieee754_bits_be_blob");

        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(THP2_2_DRIVES_SQLITE_DDL).unwrap();
        verify_projection_table_ddl(&connection, "drives", THP2_2_DRIVES_SQLITE_DDL)
            .expect("canonical drives DDL must verify");
        verify_projection_foreign_keys(&connection, "drives", &[])
            .expect("raw physical drive IDs must not invent outgoing relations");

        let unchecked = THP2_2_DRIVES_SQLITE_DDL
            .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
            .replace(
                " CHECK(start_km_f64_be IS NULL OR length(start_km_f64_be) = 8)",
                "",
            )
            .replace(" CHECK(outside_temp_avg_e1_is_nan IN (0, 1))", "");
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(&unchecked).unwrap();
        assert!(
            verify_projection_table_ddl(&connection, "drives", THP2_2_DRIVES_SQLITE_DDL).is_err(),
            "the verifier must reject a drives table recreated without exact physical checks"
        );

        let with_fk = THP2_2_DRIVES_SQLITE_DDL.replace(
            "car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767)",
            "car_id INTEGER NOT NULL REFERENCES cars(id) CHECK(car_id BETWEEN -32768 AND 32767)",
        );
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE cars (id INTEGER PRIMARY KEY) STRICT, WITHOUT ROWID;")
            .unwrap();
        connection.execute_batch(&with_fk).unwrap();
        assert!(
            verify_projection_foreign_keys(&connection, "drives", &[]).is_err(),
            "the verifier must reject invented outgoing drive relations"
        );
    }

    #[test]
    fn schema_2_2_drives_preserve_signed_open_soft_refs_and_bit_exact_values() {
        let source = snapshot_v2_2();
        assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());

        let mut signed_open = snapshot_v2_2();
        signed_open.positions.clear();
        signed_open.charges.clear();
        signed_open.addresses[0].id = i32::MIN;
        signed_open.addresses[1].id = 0;
        signed_open.geofences[0].id = -1;
        signed_open.geofences[1].id = 0;
        signed_open.drives[0].id = i32::MIN;
        signed_open.drives[0].end_date_pg_us = None;
        signed_open.drives[0].start_position_id = Some(i32::MIN);
        signed_open.drives[0].end_position_id = Some(i32::MAX);
        signed_open.drives[0].start_address_id = Some(i32::MIN);
        signed_open.drives[0].end_address_id = Some(0);
        signed_open.drives[0].start_geofence_id = Some(-1);
        signed_open.drives[0].end_geofence_id = Some(0);
        signed_open.drives[0].outside_temp_avg_e1 = Some(ProjectionFixedNumericV2_2::NaN);
        signed_open.drives[0].inside_temp_avg_e1 = None;
        signed_open.drives[0].start_km = Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits()));
        signed_open.drives[0].end_km = Some(ProjectionFloat64BitsV2_2(f64::NEG_INFINITY.to_bits()));
        signed_open.drives[0].distance = Some(ProjectionFloat64BitsV2_2(0x7ff8_0000_0000_0042));
        assert!(
            validate_request_v2_2(&request_v2_2(&signed_open), ProtocolLimits::default()).is_ok(),
            "raw signed IDs, soft selected-subset refs, open rows, NaN, NULL, and FLOAT8 bits are physical source values"
        );
        let temporary = tempfile::tempdir().unwrap();
        ProjectionPackWriter::new(temporary.path().join("signed-physical"))
            .write_full_snapshot_2_2(&request_v2_2(&signed_open))
            .expect("signed extant address/geofence IDs must survive local physical writing");

        let mut end_before_start = snapshot_v2_2();
        end_before_start.drives[0].start_date_pg_us = i64::MAX;
        end_before_start.drives[0].end_date_pg_us = Some(i64::MIN);
        assert!(
            validate_request_v2_2(&request_v2_2(&end_before_start), ProtocolLimits::default())
                .is_ok()
        );

        let mut wrong_car = snapshot_v2_2();
        wrong_car.drives[0].car_id = 11;
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&wrong_car), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "drive.car_id does not match selected_car_id"
        ));

        let mut duplicate = snapshot_v2_2();
        duplicate.drives.push(duplicate.drives[0].clone());
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&duplicate), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message)) if message == "duplicate drive.id"
        ));

        let mut invalid_numeric = snapshot_v2_2();
        invalid_numeric.drives[0].outside_temp_avg_e1 =
            Some(ProjectionFixedNumericV2_2::Finite(10_000));
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&invalid_numeric), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "drive.outside_temp_avg_e1 is outside its pinned source range"
        ));
    }

    #[test]
    fn schema_2_2_positions_contract_hash_ddl_and_zero_local_foreign_keys_are_pinned() {
        assert_eq!(
            Sha256Digest::of_bytes(THP2_2_POSITIONS_SLICE_CONTRACT.as_bytes()).to_string(),
            THP2_2_POSITIONS_SLICE_SHA256
        );
        assert_eq!(THP2_2_POSITIONS_ODOMETER_ENCODING, "ieee754_bits_be_blob");
        assert_eq!(
            THP2_2_POSITIONS_RELATION_SCOPE,
            "source_car_fk_rust_admission;source_drive_fk_omitted_cross_car_target"
        );

        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(THP2_2_POSITIONS_SQLITE_DDL)
            .unwrap();
        verify_projection_table_ddl(&connection, "positions", THP2_2_POSITIONS_SQLITE_DDL)
            .expect("canonical positions DDL must verify");
        verify_projection_foreign_keys(&connection, "positions", &[])
            .expect("the V3 local positions schema intentionally has no SQLite foreign keys");

        let unchecked = THP2_2_POSITIONS_SQLITE_DDL
            .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
            .replace(
                " CHECK(odometer_f64_be IS NULL OR length(odometer_f64_be) = 8)",
                "",
            )
            .replace(" CHECK(latitude_e6_is_nan IN (0, 1))", "");
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(&unchecked).unwrap();
        assert!(
            verify_projection_table_ddl(&connection, "positions", THP2_2_POSITIONS_SQLITE_DDL)
                .is_err(),
            "the verifier must reject a positions table recreated without physical checks"
        );

        let with_fk = THP2_2_POSITIONS_SQLITE_DDL.replace(
            "car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767)",
            "car_id INTEGER NOT NULL REFERENCES cars(id) CHECK(car_id BETWEEN -32768 AND 32767)",
        );
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE cars (id INTEGER PRIMARY KEY) STRICT, WITHOUT ROWID;")
            .unwrap();
        connection.execute_batch(&with_fk).unwrap();
        assert!(
            verify_projection_foreign_keys(&connection, "positions", &[]).is_err(),
            "the verifier must reject every foreign key in the V3 local positions schema"
        );
    }

    #[test]
    fn schema_2_2_positions_preserve_all_physical_values_without_relation_closure() {
        let source = snapshot_v2_2();
        assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());

        let temporary = tempfile::tempdir().unwrap();
        let built = ProjectionPackWriter::new(temporary.path().join("positions"))
            .write_full_snapshot_2_2(&request_v2_2(&source))
            .expect("exact physical positions must write locally");
        let inspect = temporary.path().join("positions.sqlite");
        fs::write(
            &inspect,
            zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap(),
        )
        .unwrap();
        let connection = Connection::open(&inspect).unwrap();
        #[derive(Debug, PartialEq)]
        struct PositionRow {
            id: i64,
            car_id: i64,
            drive_id: Option<i64>,
            date_pg_us: i64,
            latitude_e6: Option<i64>,
            latitude_e6_is_nan: i64,
            longitude_e6: Option<i64>,
            longitude_e6_is_nan: i64,
            elevation: Option<i64>,
            speed: Option<i64>,
            power: Option<i64>,
            odometer_f64_be: Option<Vec<u8>>,
            ideal_battery_range_km_e2: Option<i64>,
            ideal_battery_range_km_e2_is_nan: i64,
            est_battery_range_km_e2: Option<i64>,
            est_battery_range_km_e2_is_nan: i64,
            rated_battery_range_km_e2: Option<i64>,
            rated_battery_range_km_e2_is_nan: i64,
            battery_level: Option<i64>,
            usable_battery_level: Option<i64>,
            battery_heater: Option<i64>,
            battery_heater_on: Option<i64>,
            battery_heater_no_power: Option<i64>,
            outside_temp_e1: Option<i64>,
            outside_temp_e1_is_nan: i64,
            inside_temp_e1: Option<i64>,
            inside_temp_e1_is_nan: i64,
            fan_status: Option<i64>,
            driver_temp_setting_e1: Option<i64>,
            driver_temp_setting_e1_is_nan: i64,
            passenger_temp_setting_e1: Option<i64>,
            passenger_temp_setting_e1_is_nan: i64,
            is_climate_on: Option<i64>,
            is_rear_defroster_on: Option<i64>,
            is_front_defroster_on: Option<i64>,
            tpms_pressure_fl_e1: Option<i64>,
            tpms_pressure_fl_e1_is_nan: i64,
            tpms_pressure_fr_e1: Option<i64>,
            tpms_pressure_fr_e1_is_nan: i64,
            tpms_pressure_rl_e1: Option<i64>,
            tpms_pressure_rl_e1_is_nan: i64,
            tpms_pressure_rr_e1: Option<i64>,
            tpms_pressure_rr_e1_is_nan: i64,
        }
        let position = connection
            .query_row(
                "SELECT id, car_id, drive_id, date_pg_us, latitude_e6, latitude_e6_is_nan,
                        longitude_e6, longitude_e6_is_nan, elevation, speed, power,
                        odometer_f64_be, ideal_battery_range_km_e2,
                        ideal_battery_range_km_e2_is_nan, est_battery_range_km_e2,
                        est_battery_range_km_e2_is_nan, rated_battery_range_km_e2,
                        rated_battery_range_km_e2_is_nan, battery_level,
                        usable_battery_level, battery_heater, battery_heater_on,
                        battery_heater_no_power, outside_temp_e1, outside_temp_e1_is_nan,
                        inside_temp_e1, inside_temp_e1_is_nan, fan_status,
                        driver_temp_setting_e1, driver_temp_setting_e1_is_nan,
                        passenger_temp_setting_e1, passenger_temp_setting_e1_is_nan,
                        is_climate_on, is_rear_defroster_on, is_front_defroster_on,
                        tpms_pressure_fl_e1, tpms_pressure_fl_e1_is_nan,
                        tpms_pressure_fr_e1, tpms_pressure_fr_e1_is_nan,
                        tpms_pressure_rl_e1, tpms_pressure_rl_e1_is_nan,
                        tpms_pressure_rr_e1, tpms_pressure_rr_e1_is_nan
                 FROM positions WHERE id = 30",
                [],
                |row| {
                    Ok(PositionRow {
                        id: row.get(0)?,
                        car_id: row.get(1)?,
                        drive_id: row.get(2)?,
                        date_pg_us: row.get(3)?,
                        latitude_e6: row.get(4)?,
                        latitude_e6_is_nan: row.get(5)?,
                        longitude_e6: row.get(6)?,
                        longitude_e6_is_nan: row.get(7)?,
                        elevation: row.get(8)?,
                        speed: row.get(9)?,
                        power: row.get(10)?,
                        odometer_f64_be: row.get(11)?,
                        ideal_battery_range_km_e2: row.get(12)?,
                        ideal_battery_range_km_e2_is_nan: row.get(13)?,
                        est_battery_range_km_e2: row.get(14)?,
                        est_battery_range_km_e2_is_nan: row.get(15)?,
                        rated_battery_range_km_e2: row.get(16)?,
                        rated_battery_range_km_e2_is_nan: row.get(17)?,
                        battery_level: row.get(18)?,
                        usable_battery_level: row.get(19)?,
                        battery_heater: row.get(20)?,
                        battery_heater_on: row.get(21)?,
                        battery_heater_no_power: row.get(22)?,
                        outside_temp_e1: row.get(23)?,
                        outside_temp_e1_is_nan: row.get(24)?,
                        inside_temp_e1: row.get(25)?,
                        inside_temp_e1_is_nan: row.get(26)?,
                        fan_status: row.get(27)?,
                        driver_temp_setting_e1: row.get(28)?,
                        driver_temp_setting_e1_is_nan: row.get(29)?,
                        passenger_temp_setting_e1: row.get(30)?,
                        passenger_temp_setting_e1_is_nan: row.get(31)?,
                        is_climate_on: row.get(32)?,
                        is_rear_defroster_on: row.get(33)?,
                        is_front_defroster_on: row.get(34)?,
                        tpms_pressure_fl_e1: row.get(35)?,
                        tpms_pressure_fl_e1_is_nan: row.get(36)?,
                        tpms_pressure_fr_e1: row.get(37)?,
                        tpms_pressure_fr_e1_is_nan: row.get(38)?,
                        tpms_pressure_rl_e1: row.get(39)?,
                        tpms_pressure_rl_e1_is_nan: row.get(40)?,
                        tpms_pressure_rr_e1: row.get(41)?,
                        tpms_pressure_rr_e1_is_nan: row.get(42)?,
                    })
                },
            )
            .unwrap();
        assert_eq!(
            position,
            PositionRow {
                id: 30,
                car_id: 10,
                drive_id: Some(20),
                date_pg_us: 1_700_000_030_123_456,
                latitude_e6: Some(51_505_000),
                latitude_e6_is_nan: 0,
                longitude_e6: Some(-105_000),
                longitude_e6_is_nan: 0,
                elevation: Some(i64::from(i16::MIN)),
                speed: Some(i64::from(i16::MAX)),
                power: Some(i64::from(i16::MIN)),
                odometer_f64_be: Some((-0.0_f64).to_bits().to_be_bytes().to_vec()),
                ideal_battery_range_km_e2: Some(999_999),
                ideal_battery_range_km_e2_is_nan: 0,
                est_battery_range_km_e2: Some(-999_999),
                est_battery_range_km_e2_is_nan: 0,
                rated_battery_range_km_e2: None,
                rated_battery_range_km_e2_is_nan: 1,
                battery_level: Some(i64::from(i16::MIN)),
                usable_battery_level: Some(i64::from(i16::MAX)),
                battery_heater: Some(0),
                battery_heater_on: Some(1),
                battery_heater_no_power: None,
                outside_temp_e1: None,
                outside_temp_e1_is_nan: 1,
                inside_temp_e1: Some(-9_999),
                inside_temp_e1_is_nan: 0,
                fan_status: Some(i64::from(i32::MIN)),
                driver_temp_setting_e1: None,
                driver_temp_setting_e1_is_nan: 0,
                passenger_temp_setting_e1: Some(9_999),
                passenger_temp_setting_e1_is_nan: 0,
                is_climate_on: Some(1),
                is_rear_defroster_on: Some(0),
                is_front_defroster_on: None,
                tpms_pressure_fl_e1: Some(-9_999),
                tpms_pressure_fl_e1_is_nan: 0,
                tpms_pressure_fr_e1: None,
                tpms_pressure_fr_e1_is_nan: 1,
                tpms_pressure_rl_e1: None,
                tpms_pressure_rl_e1_is_nan: 0,
                tpms_pressure_rr_e1: Some(9_999),
                tpms_pressure_rr_e1_is_nan: 0,
            }
        );

        for statement in [
            "UPDATE positions SET id = 2147483648 WHERE id = 30",
            "UPDATE positions SET car_id = 32768 WHERE id = 30",
            "UPDATE positions SET date_pg_us = -9223372036854775807 WHERE id = 30",
            "UPDATE positions SET latitude_e6 = 100000000 WHERE id = 30",
            "UPDATE positions SET longitude_e6 = -1000000000 WHERE id = 30",
            "UPDATE positions SET latitude_e6 = 1, latitude_e6_is_nan = 1 WHERE id = 30",
            "UPDATE positions SET odometer_f64_be = x'00000000000000' WHERE id = 30",
            "UPDATE positions SET battery_level = 32768 WHERE id = 30",
            "UPDATE positions SET fan_status = 2147483648 WHERE id = 30",
            "UPDATE positions SET battery_heater = 2 WHERE id = 30",
            "UPDATE positions SET tpms_pressure_fl_e1 = 10000, tpms_pressure_fl_e1_is_nan = 0 WHERE id = 30",
        ] {
            assert!(
                connection.execute(statement, []).is_err(),
                "{statement} must violate exact physical positions DDL"
            );
        }

        connection
            .execute(
                "UPDATE positions SET drive_id = -2147483648, date_pg_us = 9223372036854775807,
                        latitude_e6 = 0, latitude_e6_is_nan = 0, longitude_e6 = 0,
                        longitude_e6_is_nan = 0, battery_level = -32768,
                        usable_battery_level = 32767 WHERE id = 30",
                [],
            )
            .expect(
                "an omitted cross-car drive reference and non-policy physical values are valid",
            );

        // The raw source FK can point at an extant drive of another car. That
        // target is deliberately omitted from this selected-car pack, so its
        // signed identity must remain a soft local value rather than a pack FK.
        let mut omitted_cross_car = snapshot_v2_2();
        omitted_cross_car.positions[0].id = i32::MIN;
        omitted_cross_car.positions[0].drive_id = Some(i32::MAX);
        omitted_cross_car.positions[0].date_pg_us = i64::MIN;
        omitted_cross_car.positions[0].latitude_e6 = ProjectionFixedNumericV2_2::NaN;
        omitted_cross_car.positions[0].longitude_e6 = ProjectionFixedNumericV2_2::NaN;
        omitted_cross_car.positions[0].odometer =
            Some(ProjectionFloat64BitsV2_2(0x7ff8_0000_0000_0042));
        assert!(
            validate_request_v2_2(&request_v2_2(&omitted_cross_car), ProtocolLimits::default())
                .is_ok(),
            "raw signed IDs, source timestamp infinity, NaN, and omitted cross-car drive values stay physical"
        );

        let mut wrong_car = snapshot_v2_2();
        wrong_car.positions[0].car_id = 11;
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&wrong_car), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "position.car_id does not match selected_car_id"
        ));
        let mut duplicate = snapshot_v2_2();
        duplicate.positions.push(duplicate.positions[0].clone());
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&duplicate), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message)) if message == "duplicate position.id"
        ));
        let mut invalid_numeric = snapshot_v2_2();
        invalid_numeric.positions[0].outside_temp_e1 =
            Some(ProjectionFixedNumericV2_2::Finite(10_000));
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&invalid_numeric), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "position.outside_temp_e1 is outside its pinned source range"
        ));
    }

    #[test]
    fn schema_2_2_preserves_signed_and_zero_selected_car_ids() {
        for selected_car_id in [i16::MIN, 0] {
            let mut source = snapshot_v2_2();
            source.cars[0].id = selected_car_id;
            source.drives[0].car_id = selected_car_id;
            source.positions[0].car_id = selected_car_id;
            source.charging_processes[0].car_id = selected_car_id;
            source.states[0].car_id = selected_car_id;
            source.updates[0].car_id = selected_car_id;
            let mut request = request_v2_2(&source);
            request.binding.selected_car_id = i64::from(selected_car_id);
            assert!(
                validate_request_v2_2(&request, ProtocolLimits::default()).is_ok(),
                "source smallint selected_car_id {selected_car_id} must remain physical"
            );

            let temporary = tempfile::tempdir().unwrap();
            let built = ProjectionPackWriter::new(
                temporary
                    .path()
                    .join(format!("selected-car-{selected_car_id}")),
            )
            .write_full_snapshot_2_2(&request)
            .expect("signed or zero selected car must write schema 2.2 locally");
            let inspect = temporary.path().join("selected-car.sqlite");
            fs::write(
                &inspect,
                zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap(),
            )
            .unwrap();
            let connection = Connection::open(inspect).unwrap();
            let written_car_id: i64 = connection
                .query_row("SELECT id FROM cars", [], |row| row.get(0))
                .unwrap();
            let metadata_selected_car_id: String = connection
                .query_row(
                    "SELECT value FROM hub_pack_metadata WHERE key = 'selected_car_id'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(written_car_id, i64::from(selected_car_id));
            assert_eq!(metadata_selected_car_id, selected_car_id.to_string());
        }

        let out_of_range = snapshot_v2_2();
        let mut request = request_v2_2(&out_of_range);
        request.binding.selected_car_id = i64::from(i16::MAX) + 1;
        assert!(matches!(
            validate_request_v2_2(&request, ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "schema 2.2 selected_car_id is outside the TeslaMate smallint source domain"
        ));
        assert!(matches!(
            validate_binding(&ProjectionBinding {
                selected_car_id: 0,
                ..binding()
            }),
            Err(ProjectionPackError::Invalid(message)) if message == "selected_car_id must be positive"
        ));
    }

    #[test]
    fn schema_2_2_charging_contract_hashes_ddl_and_physical_bounds_are_pinned() {
        assert_eq!(
            Sha256Digest::of_bytes(THP2_2_CHARGING_PROCESSES_SLICE_CONTRACT.as_bytes()).to_string(),
            THP2_2_CHARGING_PROCESSES_SLICE_SHA256
        );
        assert_eq!(
            Sha256Digest::of_bytes(THP2_2_CHARGES_SLICE_CONTRACT.as_bytes()).to_string(),
            THP2_2_CHARGES_SLICE_SHA256
        );
        assert_eq!(
            THP2_2_CHARGING_TRI_STATE_BOOL_ENCODING,
            "sqlite_null_or_0_or_1"
        );
        assert_eq!(
            THP2_2_CHARGES_RELATION_SCOPE,
            "charges_with_extant_selected_car_process;constraint_not_re_attested"
        );

        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(THP2_2_CHARGING_PROCESSES_SQLITE_DDL)
            .unwrap();
        connection.execute_batch(THP2_2_CHARGES_SQLITE_DDL).unwrap();
        verify_projection_table_ddl(
            &connection,
            "charging_processes",
            THP2_2_CHARGING_PROCESSES_SQLITE_DDL,
        )
        .expect("canonical charging-processes DDL must verify");
        verify_projection_table_ddl(&connection, "charges", THP2_2_CHARGES_SQLITE_DDL)
            .expect("canonical charges DDL must verify");
        verify_projection_foreign_keys(&connection, "charging_processes", &[])
            .expect("the local physical charging-process table has no outgoing FKs");
        verify_projection_foreign_keys(&connection, "charges", &[])
            .expect("the local physical charges table has no outgoing FKs");

        let unchecked_processes = THP2_2_CHARGING_PROCESSES_SQLITE_DDL
            .replace(" CHECK(position_id BETWEEN -2147483648 AND 2147483647)", "")
            .replace(" CHECK(cost_e2_is_nan IN (0, 1))", "");
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(&unchecked_processes).unwrap();
        assert!(
            verify_projection_table_ddl(
                &connection,
                "charging_processes",
                THP2_2_CHARGING_PROCESSES_SQLITE_DDL,
            )
            .is_err(),
            "the verifier must reject a charging-processes table recreated without physical checks"
        );

        let unchecked_charges = THP2_2_CHARGES_SQLITE_DDL
            .replace(" CHECK(charger_power BETWEEN -32768 AND 32767)", "")
            .replace(
                " CHECK(fast_charger_present IS NULL OR fast_charger_present IN (0, 1))",
                "",
            );
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(&unchecked_charges).unwrap();
        assert!(
            verify_projection_table_ddl(&connection, "charges", THP2_2_CHARGES_SQLITE_DDL).is_err(),
            "the verifier must reject a charges table recreated without physical checks"
        );

        let source = snapshot_v2_2();
        assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());
        let mut bad_timestamp = snapshot_v2_2();
        bad_timestamp.charging_processes[0].start_date_pg_us = i64::MIN + 1;
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&bad_timestamp), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "charging_process.start_date_pg_us is outside the PostgreSQL timestamp source domain"
        ));
        let mut bad_numeric = snapshot_v2_2();
        bad_numeric.charges[0].charge_energy_added_e2 =
            ProjectionFixedNumericV2_2::Finite(100_000_000);
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&bad_numeric), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "charge.charge_energy_added_e2 is outside its pinned source range"
        ));
        let mut bad_width = snapshot_v2_2();
        bad_width.charges[0].fast_charger_type = Some("x".repeat(256));
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&bad_width), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "charge.fast_charger_type exceeds its pinned source width"
        ));
    }

    #[test]
    fn schema_2_2_states_and_updates_contract_hashes_and_ddl_are_pinned() {
        assert_eq!(
            Sha256Digest::of_bytes(THP2_2_STATES_SLICE_CONTRACT.as_bytes()).to_string(),
            THP2_2_STATES_SLICE_SHA256
        );
        assert_eq!(
            Sha256Digest::of_bytes(THP2_2_UPDATES_SLICE_CONTRACT.as_bytes()).to_string(),
            THP2_2_UPDATES_SLICE_SHA256
        );
        assert_eq!(
            THP2_2_POSTGRES_TIMESTAMP_ENCODING,
            "postgres_timestamp_binary_i64_us_since_2000"
        );

        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(THP2_2_CAR_SETTINGS_SQLITE_DDL)
            .unwrap();
        connection.execute_batch(THP2_2_CARS_SQLITE_DDL).unwrap();
        connection.execute_batch(THP2_2_STATES_SQLITE_DDL).unwrap();
        connection.execute_batch(THP2_2_UPDATES_SQLITE_DDL).unwrap();
        verify_projection_table_ddl(&connection, "states", THP2_2_STATES_SQLITE_DDL)
            .expect("canonical states DDL must verify");
        verify_projection_table_ddl(&connection, "updates", THP2_2_UPDATES_SQLITE_DDL)
            .expect("canonical updates DDL must verify");

        let unchecked_states = THP2_2_STATES_SQLITE_DDL
            .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
            .replace(" CHECK(car_id BETWEEN -32768 AND 32767)", "")
            .replace(" CHECK(state IN ('online', 'offline', 'asleep'))", "")
            .replace(
                " CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999)",
                "",
            )
            .replace(
                " CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999)",
                "",
            );
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(THP2_2_CAR_SETTINGS_SQLITE_DDL)
            .unwrap();
        connection.execute_batch(THP2_2_CARS_SQLITE_DDL).unwrap();
        connection.execute_batch(&unchecked_states).unwrap();
        assert!(
            verify_projection_table_ddl(&connection, "states", THP2_2_STATES_SQLITE_DDL).is_err(),
            "the verifier must reject a states table recreated without physical checks"
        );

        let unchecked_updates = THP2_2_UPDATES_SQLITE_DDL
            .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
            .replace(" CHECK(car_id BETWEEN -32768 AND 32767)", "")
            .replace(
                " CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999)",
                "",
            )
            .replace(
                " CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999)",
                "",
            )
            .replace(" CHECK(version IS NULL OR length(version) <= 255)", "");
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(THP2_2_CAR_SETTINGS_SQLITE_DDL)
            .unwrap();
        connection.execute_batch(THP2_2_CARS_SQLITE_DDL).unwrap();
        connection.execute_batch(&unchecked_updates).unwrap();
        assert!(
            verify_projection_table_ddl(&connection, "updates", THP2_2_UPDATES_SQLITE_DDL).is_err(),
            "the verifier must reject an updates table recreated without physical checks"
        );
    }

    #[test]
    fn schema_2_2_verifier_rejects_table_metadata_and_foreign_key_tampering() {
        let table_names = [
            "addresses",
            "car_settings",
            "cars",
            "charges",
            "charging_processes",
            "drives",
            "geofences",
            "global_settings",
            "hub_pack_metadata",
            "positions",
            "states",
            "updates",
        ];
        for table in table_names {
            let temporary = tempfile::tempdir().unwrap();
            let source = snapshot_v2_2();
            let request = request_v2_2(&source);
            let built = ProjectionPackWriter::new(temporary.path().join("packs"))
                .write_full_snapshot_2_2(&request)
                .unwrap();
            let inspect = temporary.path().join("tampered.sqlite");
            fs::write(
                &inspect,
                zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap(),
            )
            .unwrap();
            let connection = Connection::open(&inspect).unwrap();
            connection
                .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN unexpected TEXT"))
                .unwrap();
            drop(connection);
            assert!(
                verify_projection_sqlite_2_2(&inspect, &request, built.metadata.row_count).is_err(),
                "verifier accepted a changed {table} table"
            );
        }

        let temporary = tempfile::tempdir().unwrap();
        let source = snapshot_v2_2();
        let request = request_v2_2(&source);
        let built = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot_2_2(&request)
            .unwrap();
        let inspect = temporary.path().join("metadata.sqlite");
        fs::write(
            &inspect,
            zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap(),
        )
        .unwrap();
        let connection = Connection::open(&inspect).unwrap();
        connection
            .execute(
                "INSERT INTO hub_pack_metadata (key, value) VALUES ('unexpected', 'value')",
                [],
            )
            .unwrap();
        drop(connection);
        assert!(
            verify_projection_sqlite_2_2(&inspect, &request, built.metadata.row_count).is_err(),
            "verifier accepted an extra metadata key"
        );

        let connection = Connection::open(&inspect).unwrap();
        verify_projection_foreign_keys(&connection, "drives", &[])
            .expect("exact physical drives have no outgoing SQLite foreign keys");
        verify_projection_foreign_keys(&connection, "positions", &[])
            .expect("the V3 local positions schema intentionally has no SQLite foreign keys");
        verify_projection_foreign_keys(&connection, "charging_processes", &[]).expect(
            "the V3 local charging-process schema intentionally has no SQLite foreign keys",
        );
        verify_projection_foreign_keys(&connection, "charges", &[])
            .expect("the V3 local charges schema intentionally has no SQLite foreign keys");
        for (table, expected) in [
            ("cars", vec![("car_settings", "settings_id", "id")]),
            ("states", vec![("cars", "car_id", "id")]),
            ("updates", vec![("cars", "car_id", "id")]),
        ] {
            for missing_index in 0..expected.len() {
                let mut tampered = expected.clone();
                tampered.remove(missing_index);
                assert!(
                    verify_projection_foreign_keys(&connection, table, &tampered).is_err(),
                    "verifier accepted {table} with a missing foreign key"
                );
            }
        }
    }

    #[test]
    fn schema_2_2_full_snapshot_is_deterministic_across_input_order() {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let mut first_snapshot = snapshot_v2_2();
        let mut signed_extra_drive = first_snapshot.drives[0].clone();
        signed_extra_drive.id = i32::MIN;
        signed_extra_drive.start_address_id = None;
        signed_extra_drive.end_address_id = None;
        signed_extra_drive.start_geofence_id = None;
        signed_extra_drive.end_geofence_id = None;
        first_snapshot.drives.push(signed_extra_drive);
        let mut second_snapshot = first_snapshot.clone();
        second_snapshot.cars.reverse();
        second_snapshot.car_settings.reverse();
        second_snapshot.addresses.reverse();
        second_snapshot.geofences.reverse();
        second_snapshot.drives.reverse();
        second_snapshot.positions.reverse();
        second_snapshot.charging_processes.reverse();
        second_snapshot.charges.reverse();
        second_snapshot.states.reverse();
        second_snapshot.updates.reverse();

        let first = ProjectionPackWriter::new(first_dir.path().join("packs"))
            .write_full_snapshot_2_2(&request_v2_2(&first_snapshot))
            .unwrap();
        let second = ProjectionPackWriter::new(second_dir.path().join("packs"))
            .write_full_snapshot_2_2(&request_v2_2(&second_snapshot))
            .unwrap();
        assert_eq!(first.metadata.sha256, second.metadata.sha256);
        assert_eq!(
            fs::read(first.path).unwrap(),
            fs::read(second.path).unwrap()
        );
    }

    #[test]
    fn schema_2_2_address_physical_bounds_and_source_widths_are_exact() {
        let source = snapshot_v2_2();
        assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());

        let mut independent_coordinates = snapshot_v2_2();
        independent_coordinates.addresses[0].latitude_e6 = None;
        independent_coordinates.addresses[0].longitude_e6 =
            Some(ProjectionFixedNumericV2_2::Finite(999_999_999));
        independent_coordinates.addresses[0].osm_id = Some(i64::MIN);
        assert!(
            validate_request_v2_2(
                &request_v2_2(&independent_coordinates),
                ProtocolLimits::default()
            )
            .is_ok(),
            "address physical coordinates have no geography/pair policy and osm_id has no positivity rule"
        );

        let mut display_name_at_unicode_boundary = snapshot_v2_2();
        display_name_at_unicode_boundary.addresses[0].display_name = Some("é".repeat(512));
        assert!(
            validate_request_v2_2(
                &request_v2_2(&display_name_at_unicode_boundary),
                ProtocolLimits::default()
            )
            .is_ok(),
            "PostgreSQL varchar source widths count characters, not UTF-8 bytes"
        );

        let mut overlong_display_name = snapshot_v2_2();
        overlong_display_name.addresses[0].display_name = Some("x".repeat(513));
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&overlong_display_name),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message == "address.display_name exceeds its pinned source width"
        ));

        let mut overlong_component = snapshot_v2_2();
        overlong_component.addresses[0].country = Some("x".repeat(256));
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&overlong_component), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "address.country exceeds its pinned source width"
        ));

        let mut invalid_inserted_at = snapshot_v2_2();
        invalid_inserted_at.addresses[0].inserted_at_pg_us = 1;
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&invalid_inserted_at),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message
                    == "address.inserted_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
        ));

        let mut invalid_updated_at = snapshot_v2_2();
        invalid_updated_at.addresses[0].updated_at_pg_us = -1;
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&invalid_updated_at),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message
                    == "address.updated_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
        ));

        let mut latitude_outside_source_range = snapshot_v2_2();
        latitude_outside_source_range.addresses[0].latitude_e6 =
            Some(ProjectionFixedNumericV2_2::Finite(100_000_000));
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&latitude_outside_source_range),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message == "address.latitude_e6 is outside its pinned source range"
        ));

        let mut longitude_outside_source_range = snapshot_v2_2();
        longitude_outside_source_range.addresses[0].longitude_e6 =
            Some(ProjectionFixedNumericV2_2::Finite(-1_000_000_000));
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&longitude_outside_source_range),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message == "address.longitude_e6 is outside its pinned source range"
        ));
    }

    #[test]
    fn schema_2_2_geofence_physical_bounds_and_source_widths_are_exact() {
        let source = snapshot_v2_2();
        assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());

        let mut nullable_not_applicable_nan = snapshot_v2_2();
        nullable_not_applicable_nan.geofences[0].name = String::new();
        nullable_not_applicable_nan.geofences[0].latitude_e6 = ProjectionFixedNumericV2_2::NaN;
        nullable_not_applicable_nan.geofences[0].cost_per_unit_e4 = None;
        assert!(
            validate_request_v2_2(
                &request_v2_2(&nullable_not_applicable_nan),
                ProtocolLimits::default()
            )
            .is_ok(),
            "empty varchar, numeric NaN, and nullable source numeric remain distinct physical values"
        );

        let mut unicode_boundary = snapshot_v2_2();
        unicode_boundary.geofences[0].name = "é".repeat(255);
        assert!(
            validate_request_v2_2(&request_v2_2(&unicode_boundary), ProtocolLimits::default())
                .is_ok(),
            "PostgreSQL varchar source widths count characters, not UTF-8 bytes"
        );

        let mut overlong_name = snapshot_v2_2();
        overlong_name.geofences[0].name = "x".repeat(256);
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&overlong_name), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "geofence.name exceeds its pinned source width"
        ));

        let mut invalid_inserted_at = snapshot_v2_2();
        invalid_inserted_at.geofences[0].inserted_at_pg_us = 1;
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&invalid_inserted_at),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message
                    == "geofence.inserted_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
        ));

        let mut invalid_updated_at = snapshot_v2_2();
        invalid_updated_at.geofences[0].updated_at_pg_us = i64::MAX - 1;
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&invalid_updated_at),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message
                    == "geofence.updated_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
        ));

        let mut out_of_range_finite = snapshot_v2_2();
        out_of_range_finite.geofences[0].longitude_e6 =
            ProjectionFixedNumericV2_2::Finite(1_000_000_000);
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&out_of_range_finite),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message == "geofence.longitude_e6 is outside its pinned source range"
        ));
    }

    #[test]
    fn schema_2_2_cars_and_car_settings_are_exact_selected_physical_rows() {
        let source = snapshot_v2_2();
        assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());

        let mut nullable_source_text = snapshot_v2_2();
        nullable_source_text.cars[0].vin = None;
        nullable_source_text.cars[0].name = None;
        nullable_source_text.cars[0].model = None;
        assert!(
            validate_request_v2_2(
                &request_v2_2(&nullable_source_text),
                ProtocolLimits::default()
            )
            .is_ok(),
            "physical nullable cars text must not inherit legacy required names/models"
        );

        let mut source_int8_extremes = snapshot_v2_2();
        source_int8_extremes.cars[0].eid = i64::MIN;
        source_int8_extremes.cars[0].vid = i64::MAX;
        source_int8_extremes.cars[0].settings_id = i64::MIN;
        source_int8_extremes.car_settings[0].id = i64::MIN;
        assert!(
            validate_request_v2_2(
                &request_v2_2(&source_int8_extremes),
                ProtocolLimits::default()
            )
            .is_ok(),
            "physical source bigint values have no inferred positivity policy"
        );

        let mut unicode_varchars = snapshot_v2_2();
        unicode_varchars.cars[0].model = Some("é".repeat(255));
        unicode_varchars.cars[0].marketing_name = Some("é".repeat(255));
        assert!(
            validate_request_v2_2(&request_v2_2(&unicode_varchars), ProtocolLimits::default())
                .is_ok(),
            "source varchar widths count characters rather than UTF-8 bytes"
        );

        let mut overlong_model = snapshot_v2_2();
        overlong_model.cars[0].model = Some("x".repeat(256));
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&overlong_model), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "car.model exceeds its pinned source width"
        ));

        let mut overlong_marketing_name = snapshot_v2_2();
        overlong_marketing_name.cars[0].marketing_name = Some("x".repeat(256));
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&overlong_marketing_name),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message == "car.marketing_name exceeds its pinned source width"
        ));

        let mut generic_text_cap = snapshot_v2_2();
        generic_text_cap.cars[0].vin = Some("x".repeat(MAX_TEXT_BYTES + 1));
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&generic_text_cap), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message)) if message == "car.vin is unsafe or too large"
        ));

        for (label, efficiency) in [
            ("negative-zero", -0.0_f64),
            ("positive-infinity", f64::INFINITY),
            ("negative-infinity", f64::NEG_INFINITY),
            ("nan-payload", f64::from_bits(0x7ff8_0000_0000_00a5)),
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let mut bit_exact = snapshot_v2_2();
            bit_exact.cars[0].efficiency = Some(efficiency);
            let built = ProjectionPackWriter::new(temporary.path().join(label))
                .write_full_snapshot_2_2(&request_v2_2(&bit_exact))
                .expect("FLOAT8 bit pattern is an exact physical value");
            let inspect = temporary.path().join("efficiency.sqlite");
            fs::write(
                &inspect,
                zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap(),
            )
            .unwrap();
            let connection = Connection::open(inspect).unwrap();
            let bits: Vec<u8> = connection
                .query_row("SELECT efficiency FROM cars WHERE id = 10", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                u64::from_be_bytes(bits.try_into().expect("eight-byte FLOAT8 payload")),
                efficiency.to_bits(),
                "{label} must remain bit-exact"
            );
        }

        let mut invalid_inserted_at = snapshot_v2_2();
        invalid_inserted_at.cars[0].inserted_at_pg_us = 1;
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&invalid_inserted_at),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message
                    == "car.inserted_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
        ));

        let mut invalid_updated_at = snapshot_v2_2();
        invalid_updated_at.cars[0].updated_at_pg_us = -1;
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&invalid_updated_at),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message
                    == "car.updated_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
        ));

        let mut wrong_selected_car = snapshot_v2_2();
        wrong_selected_car.cars[0].id = 11;
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&wrong_selected_car), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message)) if message == "selected_car_id does not match car.id"
        ));

        let mut missing_settings = snapshot_v2_2();
        missing_settings.car_settings.clear();
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&missing_settings), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message)) if message == "one vehicle projection must contain exactly one car_settings row"
        ));

        let mut extra_settings = snapshot_v2_2();
        extra_settings
            .car_settings
            .push(extra_settings.car_settings[0].clone());
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&extra_settings), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message)) if message == "one vehicle projection must contain exactly one car_settings row"
        ));

        let mut mismatched_settings = snapshot_v2_2();
        mismatched_settings.car_settings[0].id = 501;
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&mismatched_settings), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "car.settings_id does not match the selected car_settings.id"
        ));
    }

    #[test]
    fn schema_2_2_states_and_updates_preserve_raw_physical_values() {
        let source = snapshot_v2_2();
        assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());
        assert_eq!(source.states[0].id, i32::MIN);
        assert_eq!(source.states[0].start_date_pg_us, i64::MIN);
        assert_eq!(source.states[0].end_date_pg_us, None);
        assert_eq!(source.updates[0].id, i32::MAX);
        assert_eq!(source.updates[0].start_date_pg_us, i64::MAX);
        assert_eq!(source.updates[0].end_date_pg_us, Some(i64::MIN));

        for value in [
            i64::MIN,
            POSTGRES_TIMESTAMP_FINITE_MIN_US,
            POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US - 1,
            i64::MAX,
        ] {
            let mut boundary = snapshot_v2_2();
            boundary.states[0].start_date_pg_us = value;
            boundary.states[0].end_date_pg_us = Some(value);
            boundary.updates[0].start_date_pg_us = value;
            boundary.updates[0].end_date_pg_us = Some(value);
            assert!(
                validate_request_v2_2(&request_v2_2(&boundary), ProtocolLimits::default()).is_ok(),
                "valid PostgreSQL timestamp boundary {value} must be retained"
            );
        }
        for value in [
            i64::MIN + 1,
            POSTGRES_TIMESTAMP_FINITE_MIN_US - 1,
            POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US,
            i64::MAX - 1,
        ] {
            let mut invalid_timestamp = snapshot_v2_2();
            invalid_timestamp.states[0].start_date_pg_us = value;
            assert!(matches!(
                validate_request_v2_2(
                    &request_v2_2(&invalid_timestamp),
                    ProtocolLimits::default()
                ),
                Err(ProjectionPackError::Invalid(message))
                    if message == "state.start_date_pg_us is outside the PostgreSQL timestamp source domain"
            ));
        }

        let mut nullable = snapshot_v2_2();
        nullable.updates[0].end_date_pg_us = None;
        nullable.updates[0].version = None;
        assert!(
            validate_request_v2_2(&request_v2_2(&nullable), ProtocolLimits::default()).is_ok(),
            "nullable source end/version values must not inherit legacy completion policy"
        );

        let mut empty_version = snapshot_v2_2();
        empty_version.updates[0].version = Some(String::new());
        assert!(
            validate_request_v2_2(&request_v2_2(&empty_version), ProtocolLimits::default()).is_ok(),
            "empty source varchar is distinct from a trimmed/defaulted value"
        );

        let mut unicode_boundary = snapshot_v2_2();
        unicode_boundary.updates[0].version = Some("é".repeat(255));
        assert!(
            validate_request_v2_2(&request_v2_2(&unicode_boundary), ProtocolLimits::default())
                .is_ok(),
            "source varchar widths count characters rather than UTF-8 bytes"
        );

        let mut overlong = snapshot_v2_2();
        overlong.updates[0].version = Some("x".repeat(256));
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&overlong), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "update.version exceeds its pinned source width"
        ));

        let mut unsafe_text = snapshot_v2_2();
        unsafe_text.updates[0].version = Some("x".repeat(MAX_TEXT_BYTES + 1));
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&unsafe_text), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "update.version is unsafe or too large"
        ));

        let mut wrong_car = snapshot_v2_2();
        wrong_car.states[0].car_id = 11;
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&wrong_car), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message))
                if message == "state.car_id does not match selected car"
        ));

        let mut duplicate = snapshot_v2_2();
        duplicate.states.push(duplicate.states[0].clone());
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&duplicate), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message)) if message == "state.id is duplicated"
        ));

        let mut duplicate_update = snapshot_v2_2();
        duplicate_update
            .updates
            .push(duplicate_update.updates[0].clone());
        assert!(matches!(
            validate_request_v2_2(&request_v2_2(&duplicate_update), ProtocolLimits::default()),
            Err(ProjectionPackError::Invalid(message)) if message == "update.id is duplicated"
        ));
    }

    #[test]
    fn schema_2_2_keeps_source_refs_soft_and_enforces_charge_process_scope() {
        let temporary = tempfile::tempdir().unwrap();

        // Exact source reference values stay soft locally: source targets can
        // be extant but omitted from a selected-car subset, so V3 does not
        // invent SQLite closure relations.
        let packs = temporary.path().join("soft-source-refs");
        let mut soft_refs = snapshot_v2_2();
        soft_refs.positions.clear();
        soft_refs.addresses.clear();
        soft_refs.geofences.clear();
        soft_refs.charging_processes[0].position_id = i32::MIN;
        soft_refs.charging_processes[0].address_id = Some(i32::MIN);
        soft_refs.charging_processes[0].geofence_id = Some(i32::MAX);
        soft_refs.drives[0].start_position_id = Some(i32::MIN);
        soft_refs.drives[0].end_position_id = Some(i32::MAX);
        soft_refs.drives[0].start_address_id = Some(i32::MIN);
        soft_refs.drives[0].end_address_id = Some(i32::MAX);
        soft_refs.drives[0].start_geofence_id = Some(i32::MIN);
        soft_refs.drives[0].end_geofence_id = Some(i32::MAX);
        ProjectionPackWriter::new(&packs)
            .write_full_snapshot_2_2(&request_v2_2(&soft_refs))
            .expect("raw signed source references remain physical selected-subset values");

        let packs = temporary.path().join("charge-process-closure");
        let mut missing_process = snapshot_v2_2();
        missing_process.charges[0].charging_process_id = i32::MIN;
        let error = ProjectionPackWriter::new(&packs)
            .write_full_snapshot_2_2(&request_v2_2(&missing_process))
            .expect_err("selected-car charges require their loaded source process");
        assert!(matches!(
            error,
            ProjectionPackError::Invalid(message)
                if message == "charge.charging_process_id is not present in this local physical slice"
        ));
        assert!(!packs.exists());

        let packs = temporary.path().join("unreferenced-relation");
        let mut unreferenced = snapshot_v2_2();
        unreferenced.addresses.push(ProjectionAddressV2_2 {
            id: 102,
            display_name: Some("not selected-car-referenced".into()),
            latitude_e6: None,
            longitude_e6: None,
            name: None,
            house_number: None,
            road: None,
            neighbourhood: None,
            city: None,
            county: None,
            postcode: None,
            state: None,
            state_district: None,
            country: None,
            inserted_at_pg_us: 1_700_000_400_000_000,
            updated_at_pg_us: 1_700_000_500_000_000,
            osm_id: None,
            osm_type: None,
        });
        let error = ProjectionPackWriter::new(&packs)
            .write_full_snapshot_2_2(&request_v2_2(&unreferenced))
            .expect_err("account-wide relation rows are out of scope");
        assert!(matches!(
            error,
            ProjectionPackError::Invalid(message)
                if message.contains("address 102 is not referenced by the selected car")
        ));
        assert!(!packs.exists());
    }

    #[test]
    fn schema_2_0_pack_matches_released_client_layout() {
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
        let mut tables = connection
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                 ORDER BY name",
            )
            .unwrap();
        let tables = tables
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            tables,
            vec![
                "cars",
                "charge_samples",
                "charges",
                "drives",
                "hub_pack_metadata",
                "positions",
            ]
        );
        let user_version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(user_version, HUB_PROJECTION_SCHEMA_V1.sqlite_user_version());
        assert_schema_2_0_client_layout(&connection);
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
    fn typed_delta_rejects_blank_update_version_before_writing_a_pack() {
        let temporary = tempfile::tempdir().unwrap();
        let mut delta = sparse_delta();
        delta.updates[0].version.clear();

        let error = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_delta(&delta_request(&delta))
            .expect_err("blank update versions are not a valid typed-delta payload");
        assert!(matches!(
            error,
            ProjectionPackError::Invalid(message) if message.contains("update.version must not be empty")
        ));
        assert!(
            !temporary.path().join("packs").exists(),
            "validation must reject before creating an immutable pack directory"
        );
    }

    #[test]
    fn typed_delta_rejects_unsupported_source_owned_tombstones_before_writing_a_pack() {
        for entity in [
            ProjectionDeltaEntity::Car,
            ProjectionDeltaEntity::CarSetting,
            ProjectionDeltaEntity::Geofence,
            ProjectionDeltaEntity::Address,
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let packs = temporary.path().join("packs");
            let mut delta = sparse_delta();
            delta.tombstones = vec![ProjectionTombstone {
                entity,
                id: 999,
                car_id: delta.binding.selected_car_id,
            }];

            let error = ProjectionPackWriter::new(&packs)
                .write_delta(&delta_request(&delta))
                .expect_err("unsupported source-owned tombstones must be rejected");
            assert!(matches!(
                error,
                ProjectionPackError::Invalid(message)
                    if message.contains("unsupported source-owned delta tombstone entity")
            ));
            assert!(
                !packs.exists(),
                "validation must reject before creating an immutable pack directory"
            );
        }
    }

    #[test]
    fn typed_delta_rejects_upsert_tombstone_overlap_before_writing_a_pack() {
        for entity in [
            ProjectionDeltaEntity::Drive,
            ProjectionDeltaEntity::Position,
            ProjectionDeltaEntity::Charge,
            ProjectionDeltaEntity::ChargeSample,
            ProjectionDeltaEntity::State,
            ProjectionDeltaEntity::Update,
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let packs = temporary.path().join("packs");
            let mut delta = sparse_delta();
            let id = match entity {
                ProjectionDeltaEntity::Drive => delta.drives[0].id,
                ProjectionDeltaEntity::Position => delta.positions[0].id,
                ProjectionDeltaEntity::Charge => {
                    let charge = snapshot().charges.into_iter().next().unwrap();
                    let id = charge.id;
                    delta.charges.push(charge);
                    id
                }
                ProjectionDeltaEntity::ChargeSample => {
                    let sample = snapshot().charge_samples.into_iter().next().unwrap();
                    let id = sample.id;
                    delta.charge_samples.push(sample);
                    id
                }
                ProjectionDeltaEntity::State => delta.states[0].id,
                ProjectionDeltaEntity::Update => delta.updates[0].id,
                ProjectionDeltaEntity::Car
                | ProjectionDeltaEntity::CarSetting
                | ProjectionDeltaEntity::Geofence
                | ProjectionDeltaEntity::Address => unreachable!("supported tombstone entity"),
            };
            delta.tombstones = vec![ProjectionTombstone {
                entity,
                id,
                car_id: delta.binding.selected_car_id,
            }];

            let error = ProjectionPackWriter::new(&packs)
                .write_delta(&delta_request(&delta))
                .expect_err("a typed row cannot be upserted and tombstoned together");
            assert!(matches!(
                error,
                ProjectionPackError::Invalid(message)
                    if message == "typed delta upsert and tombstone overlap"
            ));
            assert!(
                !packs.exists(),
                "validation must reject before creating an immutable pack directory"
            );
        }
    }

    #[test]
    fn source_owned_tombstone_canonical_order_is_child_first() {
        let tombstones = vec![
            ProjectionTombstone {
                entity: ProjectionDeltaEntity::Drive,
                id: 20,
                car_id: 10,
            },
            ProjectionTombstone {
                entity: ProjectionDeltaEntity::Charge,
                id: 40,
                car_id: 10,
            },
            ProjectionTombstone {
                entity: ProjectionDeltaEntity::Position,
                id: 30,
                car_id: 10,
            },
            ProjectionTombstone {
                entity: ProjectionDeltaEntity::ChargeSample,
                id: 50,
                car_id: 10,
            },
            ProjectionTombstone {
                entity: ProjectionDeltaEntity::Update,
                id: 70,
                car_id: 10,
            },
            ProjectionTombstone {
                entity: ProjectionDeltaEntity::State,
                id: 60,
                car_id: 10,
            },
        ];

        let entities = source_owned_tombstones_in_canonical_order(&tombstones)
            .into_iter()
            .map(|row| row.entity)
            .collect::<Vec<_>>();
        assert_eq!(
            entities,
            vec![
                ProjectionDeltaEntity::ChargeSample,
                ProjectionDeltaEntity::Position,
                ProjectionDeltaEntity::Charge,
                ProjectionDeltaEntity::Drive,
                ProjectionDeltaEntity::State,
                ProjectionDeltaEntity::Update,
            ]
        );
    }

    #[test]
    fn invalid_car_settings_reject_before_pack_output_in_every_writer_path() {
        let temporary = tempfile::tempdir().unwrap();
        let packs = temporary.path().join("full-v1");
        let mut source = snapshot();
        source.cars[0].settings.suspend_after_idle_min = 0;
        let error = ProjectionPackWriter::new(&packs)
            .write_full_snapshot(&request(&source))
            .expect_err("full snapshots reject invalid embedded car settings");
        assert!(matches!(
            error,
            ProjectionPackError::Invalid(message) if message == "car settings durations must be positive"
        ));
        assert!(!packs.exists());

        let temporary = tempfile::tempdir().unwrap();
        let packs = temporary.path().join("full-v2");
        let mut source = snapshot();
        source.cars[0].settings.suspend_min = 0;
        let error = ProjectionPackWriter::new(&packs)
            .write_full_snapshot_with_states(&request(&source), &[])
            .expect_err("stateful full snapshots reject invalid embedded car settings");
        assert!(matches!(
            error,
            ProjectionPackError::Invalid(message) if message == "car settings durations must be positive"
        ));
        assert!(!packs.exists());

        let temporary = tempfile::tempdir().unwrap();
        let packs = temporary.path().join("delta-car");
        let mut delta = sparse_delta();
        delta.cars[0].settings.suspend_min = 0;
        let error = ProjectionPackWriter::new(&packs)
            .write_delta(&delta_request(&delta))
            .expect_err("car delta upserts reject invalid embedded settings");
        assert!(matches!(
            error,
            ProjectionPackError::Invalid(message) if message == "car settings durations must be positive"
        ));
        assert!(!packs.exists());

        let temporary = tempfile::tempdir().unwrap();
        let packs = temporary.path().join("delta-patch");
        let mut delta = sparse_delta();
        delta.cars.clear();
        delta.car_settings = vec![ProjectionCarSettingsPatch {
            car_id: delta.binding.selected_car_id,
            settings: ProjectionCarSettings {
                suspend_after_idle_min: 0,
                ..ProjectionCarSettings::default()
            },
        }];
        let error = ProjectionPackWriter::new(&packs)
            .write_delta(&delta_request(&delta))
            .expect_err("settings-only delta patches reject invalid settings");
        assert!(matches!(
            error,
            ProjectionPackError::Invalid(message) if message == "car settings durations must be positive"
        ));
        assert!(!packs.exists());
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
            .query_row(
                "SELECT value FROM hub_pack_metadata WHERE key = 'mode'",
                [],
                |row| row.get(0),
            )
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
            .query_row(
                "SELECT entity, entity_id, car_id FROM tombstones",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
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
        snapshot_id: Uuid,
    ) -> ProjectionDeltaPackRequest<'a> {
        ProjectionDeltaPackRequest {
            pack_id: Uuid::parse_str(pack_id).unwrap(),
            snapshot_id,
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
                base_request.snapshot_id,
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
                base_request.snapshot_id,
            ))
            .unwrap();

        let key = CursorKey::from_bytes([42; 32]);
        let chain_one = Sha256Digest::of_bytes(
            format!(
                "delta-v2/{}:{}",
                base.metadata.sha256, first.metadata.sha256
            )
            .as_bytes(),
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
        files.push((
            "manifest.json".into(),
            [manifest_bytes, b"\n".to_vec()].concat(),
        ));
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
        claims.push(format!(
            "{}  {} manifest.json",
            digest,
            manifest_bytes.len()
        ));
        claims.sort();
        fs::write(root.join("SHA256SUMS"), format!("{}\n", claims.join("\n"))).unwrap();
        fs::remove_dir_all(root.join("work")).unwrap();
    }

    // These frozen pack bytes were generated by the macOS SQLite/zstd toolchain.
    // Linux validates the same schema and lineage through the portable pack tests.
    #[cfg(target_os = "macos")]
    #[test]
    fn delta_v2_fixtures_regenerate_deterministically_and_validate_lineage() {
        let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/delta-v2");
        let (manifest, expected_files) =
            fixture_lineage(&tempfile::tempdir().unwrap().path().join("work"));
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
            serde_json::from_slice(&fs::read(fixture_root.join("manifest.json")).unwrap()).unwrap();
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
