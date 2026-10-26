-- SPDX-License-Identifier: AGPL-3.0-only
-- Reproduces the Teslatlas Protocol hub-sync-v1@1.4.0 contract physical-delta-pack-v1.sql, published under Apache-2.0.
-- Source-neutral PhysicalV3 changed-set pack format v1.

-- The field catalog defines full source rows; this schema adds operation and closure evidence.

CREATE TABLE delta_metadata (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT, WITHOUT ROWID;

CREATE TABLE global_settings (
    id INTEGER PRIMARY KEY NOT NULL,
    unit_of_length TEXT NOT NULL,
    unit_of_temperature TEXT NOT NULL,
    unit_of_pressure TEXT NOT NULL,
    preferred_range TEXT NOT NULL,
    base_url TEXT,
    grafana_url TEXT,
    language TEXT NOT NULL,
    theme_mode TEXT NOT NULL,
    inserted_at_pg_us INTEGER NOT NULL,
    updated_at_pg_us INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE car_settings (
    id INTEGER PRIMARY KEY NOT NULL,
    suspend_min INTEGER NOT NULL,
    suspend_after_idle_min INTEGER NOT NULL,
    req_not_unlocked INTEGER NOT NULL,
    free_supercharging INTEGER NOT NULL,
    use_streaming_api INTEGER NOT NULL,
    enabled INTEGER NOT NULL,
    lfp_battery INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE cars (
    id INTEGER PRIMARY KEY NOT NULL,
    eid INTEGER NOT NULL,
    vid INTEGER NOT NULL,
    vin TEXT,
    name TEXT,
    model TEXT,
    efficiency BLOB,
    trim_badging TEXT,
    marketing_name TEXT,
    exterior_color TEXT,
    wheel_type TEXT,
    spoiler_type TEXT,
    display_priority INTEGER NOT NULL,
    inserted_at_pg_us INTEGER NOT NULL,
    updated_at_pg_us INTEGER NOT NULL,
    settings_id INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE drives (
    id INTEGER PRIMARY KEY NOT NULL,
    car_id INTEGER NOT NULL,
    start_date_pg_us INTEGER NOT NULL,
    end_date_pg_us INTEGER,
    start_position_id INTEGER,
    end_position_id INTEGER,
    start_address_id INTEGER,
    end_address_id INTEGER,
    start_geofence_id INTEGER,
    end_geofence_id INTEGER,
    outside_temp_avg_e1 INTEGER,
    outside_temp_avg_e1_is_nan INTEGER NOT NULL,
    inside_temp_avg_e1 INTEGER,
    inside_temp_avg_e1_is_nan INTEGER NOT NULL,
    speed_max INTEGER,
    power_max INTEGER,
    power_min INTEGER,
    start_ideal_range_km_e2 INTEGER,
    start_ideal_range_km_e2_is_nan INTEGER NOT NULL,
    end_ideal_range_km_e2 INTEGER,
    end_ideal_range_km_e2_is_nan INTEGER NOT NULL,
    start_rated_range_km_e2 INTEGER,
    start_rated_range_km_e2_is_nan INTEGER NOT NULL,
    end_rated_range_km_e2 INTEGER,
    end_rated_range_km_e2_is_nan INTEGER NOT NULL,
    start_km_f64_be BLOB,
    end_km_f64_be BLOB,
    distance_f64_be BLOB,
    duration_min INTEGER,
    ascent INTEGER,
    descent INTEGER
) STRICT, WITHOUT ROWID;

CREATE TABLE positions (
    id INTEGER PRIMARY KEY NOT NULL,
    car_id INTEGER NOT NULL,
    drive_id INTEGER,
    date_pg_us INTEGER NOT NULL,
    latitude_e6 INTEGER,
    latitude_e6_is_nan INTEGER NOT NULL,
    longitude_e6 INTEGER,
    longitude_e6_is_nan INTEGER NOT NULL,
    elevation INTEGER,
    speed INTEGER,
    power INTEGER,
    odometer_f64_be BLOB,
    ideal_battery_range_km_e2 INTEGER,
    ideal_battery_range_km_e2_is_nan INTEGER NOT NULL,
    est_battery_range_km_e2 INTEGER,
    est_battery_range_km_e2_is_nan INTEGER NOT NULL,
    rated_battery_range_km_e2 INTEGER,
    rated_battery_range_km_e2_is_nan INTEGER NOT NULL,
    battery_level INTEGER,
    usable_battery_level INTEGER,
    battery_heater INTEGER,
    battery_heater_on INTEGER,
    battery_heater_no_power INTEGER,
    outside_temp_e1 INTEGER,
    outside_temp_e1_is_nan INTEGER NOT NULL,
    inside_temp_e1 INTEGER,
    inside_temp_e1_is_nan INTEGER NOT NULL,
    fan_status INTEGER,
    driver_temp_setting_e1 INTEGER,
    driver_temp_setting_e1_is_nan INTEGER NOT NULL,
    passenger_temp_setting_e1 INTEGER,
    passenger_temp_setting_e1_is_nan INTEGER NOT NULL,
    is_climate_on INTEGER,
    is_rear_defroster_on INTEGER,
    is_front_defroster_on INTEGER,
    tpms_pressure_fl_e1 INTEGER,
    tpms_pressure_fl_e1_is_nan INTEGER NOT NULL,
    tpms_pressure_fr_e1 INTEGER,
    tpms_pressure_fr_e1_is_nan INTEGER NOT NULL,
    tpms_pressure_rl_e1 INTEGER,
    tpms_pressure_rl_e1_is_nan INTEGER NOT NULL,
    tpms_pressure_rr_e1 INTEGER,
    tpms_pressure_rr_e1_is_nan INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE charging_processes (
    id INTEGER PRIMARY KEY NOT NULL,
    car_id INTEGER NOT NULL,
    position_id INTEGER NOT NULL,
    address_id INTEGER,
    geofence_id INTEGER,
    start_date_pg_us INTEGER NOT NULL,
    end_date_pg_us INTEGER,
    charge_energy_added_e2 INTEGER,
    charge_energy_added_e2_is_nan INTEGER NOT NULL,
    charge_energy_used_e2 INTEGER,
    charge_energy_used_e2_is_nan INTEGER NOT NULL,
    start_ideal_range_km_e2 INTEGER,
    start_ideal_range_km_e2_is_nan INTEGER NOT NULL,
    end_ideal_range_km_e2 INTEGER,
    end_ideal_range_km_e2_is_nan INTEGER NOT NULL,
    start_rated_range_km_e2 INTEGER,
    start_rated_range_km_e2_is_nan INTEGER NOT NULL,
    end_rated_range_km_e2 INTEGER,
    end_rated_range_km_e2_is_nan INTEGER NOT NULL,
    start_battery_level INTEGER,
    end_battery_level INTEGER,
    duration_min INTEGER,
    outside_temp_avg_e1 INTEGER,
    outside_temp_avg_e1_is_nan INTEGER NOT NULL,
    cost_e2 INTEGER,
    cost_e2_is_nan INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE charges (
    id INTEGER PRIMARY KEY NOT NULL,
    charging_process_id INTEGER NOT NULL,
    date_pg_us INTEGER NOT NULL,
    battery_heater INTEGER,
    battery_heater_on INTEGER,
    battery_heater_no_power INTEGER,
    battery_level INTEGER,
    usable_battery_level INTEGER,
    charge_energy_added_e2 INTEGER,
    charge_energy_added_e2_is_nan INTEGER NOT NULL,
    charger_actual_current INTEGER,
    charger_phases INTEGER,
    charger_pilot_current INTEGER,
    charger_power INTEGER NOT NULL,
    charger_voltage INTEGER,
    conn_charge_cable TEXT,
    fast_charger_present INTEGER,
    fast_charger_brand TEXT,
    fast_charger_type TEXT,
    ideal_battery_range_km_e2 INTEGER,
    ideal_battery_range_km_e2_is_nan INTEGER NOT NULL,
    rated_battery_range_km_e2 INTEGER,
    rated_battery_range_km_e2_is_nan INTEGER NOT NULL,
    not_enough_power_to_heat INTEGER,
    outside_temp_e1 INTEGER,
    outside_temp_e1_is_nan INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE addresses (
    id INTEGER PRIMARY KEY NOT NULL,
    display_name TEXT,
    latitude_e6 INTEGER,
    latitude_e6_is_nan INTEGER NOT NULL,
    longitude_e6 INTEGER,
    longitude_e6_is_nan INTEGER NOT NULL,
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
    inserted_at_pg_us INTEGER NOT NULL,
    updated_at_pg_us INTEGER NOT NULL,
    osm_id INTEGER,
    osm_type TEXT
) STRICT, WITHOUT ROWID;

CREATE TABLE geofences (
    id INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    latitude_e6 INTEGER,
    latitude_e6_is_nan INTEGER NOT NULL,
    longitude_e6 INTEGER,
    longitude_e6_is_nan INTEGER NOT NULL,
    radius INTEGER NOT NULL,
    billing_type TEXT NOT NULL,
    cost_per_unit_e4 INTEGER,
    cost_per_unit_e4_is_nan INTEGER NOT NULL,
    session_fee_e2 INTEGER,
    session_fee_e2_is_nan INTEGER NOT NULL,
    inserted_at_pg_us INTEGER NOT NULL,
    updated_at_pg_us INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE states (
    id INTEGER PRIMARY KEY NOT NULL,
    car_id INTEGER NOT NULL,
    state TEXT NOT NULL,
    start_date_pg_us INTEGER NOT NULL,
    end_date_pg_us INTEGER
) STRICT, WITHOUT ROWID;

CREATE TABLE updates (
    id INTEGER PRIMARY KEY NOT NULL,
    car_id INTEGER NOT NULL,
    start_date_pg_us INTEGER NOT NULL,
    end_date_pg_us INTEGER,
    version TEXT
) STRICT, WITHOUT ROWID;

CREATE TABLE row_roles (table_name TEXT NOT NULL CHECK(table_name IN ('global_settings', 'car_settings', 'cars', 'drives', 'positions', 'charging_processes', 'charges', 'addresses', 'geofences', 'states', 'updates')), entity_id INTEGER NOT NULL, role TEXT NOT NULL CHECK(role IN ('changed', 'context')), PRIMARY KEY(table_name, entity_id)) STRICT, WITHOUT ROWID;

CREATE TABLE tombstones (table_name TEXT NOT NULL CHECK(table_name IN ('global_settings', 'car_settings', 'cars', 'drives', 'positions', 'charging_processes', 'charges', 'addresses', 'geofences', 'states', 'updates')), entity_id INTEGER NOT NULL, PRIMARY KEY(table_name, entity_id)) STRICT, WITHOUT ROWID;

CREATE TABLE affected_projected_ids (table_name TEXT NOT NULL CHECK(table_name IN ('cars', 'car_settings', 'drives', 'positions', 'charges', 'charge_samples', 'car_states', 'car_updates')), entity_id INTEGER NOT NULL, effect TEXT NOT NULL CHECK(effect IN ('recompute', 'delete')), PRIMARY KEY(table_name, entity_id)) STRICT, WITHOUT ROWID;

CREATE TABLE impacted_roots (root_type TEXT NOT NULL CHECK(root_type IN ('drive', 'charge')), root_id INTEGER NOT NULL, base_state TEXT NOT NULL CHECK(base_state IN ('absent', 'open', 'closed')), target_state TEXT NOT NULL CHECK(target_state IN ('absent', 'open', 'closed')), target_child_count INTEGER NOT NULL CHECK(target_child_count >= 0), PRIMARY KEY(root_type, root_id)) STRICT, WITHOUT ROWID;
