-- Deterministic Hub-owned PostgreSQL source fixture.  It models the reviewed
-- TeslaMate v4.1 reader contract, not a runnable TeslaMate deployment.
CREATE TABLE schema_migrations (
  version bigint PRIMARY KEY
);
INSERT INTO schema_migrations (version) VALUES (20260411070212);

CREATE TYPE states_status AS ENUM (
  'asleep', 'offline', 'online', 'driving', 'charging', 'updating', 'unknown'
);

CREATE TABLE cars (
  id smallint PRIMARY KEY, eid bigint NOT NULL, vid bigint NOT NULL,
  vin text, name text, model text, efficiency double precision,
  trim_badging text, marketing_name text, exterior_color text, wheel_type text,
  spoiler_type text, display_priority smallint NOT NULL,
  inserted_at timestamp NOT NULL, updated_at timestamp NOT NULL
);

CREATE TABLE drives (
  id integer PRIMARY KEY, car_id smallint NOT NULL, start_date timestamp NOT NULL,
  end_date timestamp, start_position_id integer, end_position_id integer,
  start_address_id integer, end_address_id integer, start_geofence_id integer,
  end_geofence_id integer, outside_temp_avg numeric, inside_temp_avg numeric,
  speed_max smallint, power_max smallint, power_min smallint,
  start_ideal_range_km numeric, end_ideal_range_km numeric,
  start_rated_range_km numeric, end_rated_range_km numeric, start_km double precision,
  end_km double precision, distance double precision, duration_min smallint,
  ascent smallint, descent smallint
);

CREATE TABLE positions (
  id integer PRIMARY KEY, car_id smallint NOT NULL, drive_id integer,
  date timestamp NOT NULL, latitude numeric NOT NULL, longitude numeric NOT NULL,
  elevation smallint, speed smallint, power smallint, odometer double precision,
  ideal_battery_range_km numeric, est_battery_range_km numeric,
  rated_battery_range_km numeric, battery_level smallint,
  usable_battery_level smallint, battery_heater boolean, battery_heater_on boolean,
  battery_heater_no_power boolean, outside_temp numeric, inside_temp numeric,
  fan_status integer, driver_temp_setting numeric, passenger_temp_setting numeric,
  is_climate_on boolean, is_rear_defroster_on boolean, is_front_defroster_on boolean,
  tpms_pressure_fl numeric, tpms_pressure_fr numeric, tpms_pressure_rl numeric,
  tpms_pressure_rr numeric
);

CREATE TABLE charging_processes (
  id integer PRIMARY KEY, car_id smallint NOT NULL, position_id integer NOT NULL,
  address_id integer, geofence_id integer, start_date timestamp NOT NULL,
  end_date timestamp, charge_energy_added numeric, charge_energy_used numeric,
  start_ideal_range_km numeric, end_ideal_range_km numeric,
  start_rated_range_km numeric, end_rated_range_km numeric,
  start_battery_level smallint, end_battery_level smallint, duration_min smallint,
  outside_temp_avg numeric, cost numeric
);

CREATE TABLE charges (
  id integer PRIMARY KEY, charging_process_id integer NOT NULL, date timestamp NOT NULL,
  battery_heater boolean, battery_heater_on boolean, battery_heater_no_power boolean,
  battery_level smallint, usable_battery_level smallint,
  charge_energy_added numeric NOT NULL, charger_actual_current smallint,
  charger_phases smallint, charger_pilot_current smallint, charger_power smallint NOT NULL,
  charger_voltage smallint, conn_charge_cable text, fast_charger_present boolean,
  fast_charger_brand text, fast_charger_type text, ideal_battery_range_km numeric NOT NULL,
  rated_battery_range_km numeric, not_enough_power_to_heat boolean, outside_temp numeric
);

CREATE TABLE addresses (
  id integer PRIMARY KEY, display_name text, name text
);

CREATE TABLE geofences (
  id integer PRIMARY KEY, name text NOT NULL
);

CREATE TABLE states (
  id integer PRIMARY KEY, car_id smallint NOT NULL, state states_status NOT NULL,
  start_date timestamp NOT NULL, end_date timestamp
);

CREATE TABLE updates (
  id integer PRIMARY KEY, car_id smallint NOT NULL, start_date timestamp NOT NULL,
  end_date timestamp, version text
);
