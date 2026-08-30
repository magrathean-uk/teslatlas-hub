// SPDX-License-Identifier: AGPL-3.0-only

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
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
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
const PRIVATE_STAGING_DIRECTORY_MODE: u32 = 0o700;
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

include!("hub_pack/model.rs");
include!("hub_pack/manifest.rs");
include!("hub_pack/writer.rs");
include!("hub_pack/validation.rs");
include!("hub_pack/sqlite_writers.rs");
include!("hub_pack/sqlite_verification.rs");
include!("hub_pack/sqlite_v2_2.rs");
include!("hub_pack/sqlite_legacy.rs");
include!("hub_pack/compression.rs");
include!("hub_pack/validation_helpers.rs");
include!("hub_pack/staging_and_errors.rs");

#[cfg(test)]
#[path = "hub_pack/tests.rs"]
mod tests;
