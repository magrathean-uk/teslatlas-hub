// C6-C2 focused checks of the generated client (C6 R47 to R49, R58, R59; the work order's
// "Focused TypeScript checks"). The compile-time witnesses are checked by `npm run typecheck`
// (vitest does not type-check), the runtime values by this file's four named tests.
import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";
import { ProtocolValidationError } from "../../src/core/errors.js";
import type { components } from "../../src/generated/hub-protocol.js";
import { decodeHubJson, validateHubCurrent, validateHubDrives } from "../../src/hub/validate.js";

type Schemas = components["schemas"];
type Current = Schemas["current"];
type Car = NonNullable<Current["car"]>;
type Settings = Car["settings"];
type Drive = Schemas["drive"];
type Charge = Schemas["charge"];
type Position = Schemas["position"];
type ChargeSample = Schemas["charge_sample"];

// ------------------------------------------------------------------ type witnesses

type Equal<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
type Check<T extends true> = T;
// An open (index signature) type would let an absent renamed key type-check as `unknown`.
type Closed<T> = string extends keyof T ? false : true;
// Each pair [1.2.0 key, 1.3.0 key]: both keys are declared and hold the same type.
type Same<C, P extends readonly [string, string]> = P extends readonly [
  infer Old extends string,
  infer New extends string,
]
  ? Old extends keyof C
    ? New extends keyof C
      ? Equal<C[Old], C[New]>
      : false
    : false
  : never;
// Each key is declared with exactly the stated type.
type Typed<C, E> = {
  [K in keyof E]: K extends keyof C ? Equal<C[K], E[K]> : false;
}[keyof E];

const currentRenames = [
  ["state", "activity_state"],
  ["since", "state_since_ms"],
  ["healthy", "processing_healthy"],
  ["geofence", "geofence_name"],
  ["model", "model_code"],
  ["version", "software_version"],
  ["update_available", "software_update_available"],
  ["update_status", "software_update_status"],
  ["update_version", "software_update_version"],
  ["download_perc", "software_update_download_perc"],
  ["install_perc", "software_update_install_perc"],
  ["speed", "speed_kmh"],
  ["odometer", "odometer_km"],
  ["rated_battery_range_km", "battery_range_km"],
  ["active_route_miles_to_arrival", "active_route_km_to_arrival"],
  ["plugged_in", "charge_cable_connected"],
  ["doors_open", "any_door_open"],
  ["windows_open", "any_window_open"],
  ["driver_front_door_open", "door_driver_front_open"],
  ["driver_rear_door_open", "door_driver_rear_open"],
  ["passenger_front_door_open", "door_passenger_front_open"],
  ["passenger_rear_door_open", "door_passenger_rear_open"],
  ["driver_front_window_open", "window_driver_front_open"],
  ["driver_rear_window_open", "window_driver_rear_open"],
  ["passenger_front_window_open", "window_passenger_front_open"],
  ["passenger_rear_window_open", "window_passenger_rear_open"],
  ["frunk_open", "front_trunk_open"],
  ["trunk_open", "rear_trunk_open"],
] as const;
const carRenames = [
  ["marketing_name", "variant_name"],
  ["source_eid", "api_vehicle_id"],
  ["source_vid", "stream_vehicle_id"],
] as const;
// `car.model` (a non-null stored text) and `car.model_code` (a nullable Hub code) are not the
// same type: the alias keeps what 1.2.0 served (C6 R50).
const carModelPair = ["model", "model_code"] as const;
const settingsRenames = [
  ["enabled", "collection_enabled"],
  ["use_streaming_api", "streaming_enabled"],
  ["suspend_min", "sleep_window_min"],
  ["suspend_after_idle_min", "sleep_after_idle_min"],
  ["suspend_min_resolved", "sleep_window_min_resolved"],
  ["req_not_unlocked", "sleep_requires_locked"],
  ["free_supercharging", "supercharging_free"],
  ["lfp_battery", "battery_lfp"],
] as const;
const driveRenames = [
  ["start_date_ms", "started_at_ms"],
  ["end_date_ms", "ended_at_ms"],
  ["start_ideal_range_km", "start_ideal_battery_range_km"],
  ["end_ideal_range_km", "end_ideal_battery_range_km"],
  ["start_rated_range_km", "start_battery_range_km"],
  ["end_rated_range_km", "end_battery_range_km"],
  ["start_geofence", "start_geofence_name"],
  ["end_geofence", "end_geofence_name"],
  ["speed_max", "speed_max_kmh"],
  ["power_max", "power_max_kw"],
  ["power_min", "power_min_kw"],
  ["outside_temp_avg", "outside_temp_avg_c"],
  ["inside_temp_avg", "inside_temp_avg_c"],
  ["ascent", "ascent_m"],
  ["descent", "descent_m"],
] as const;
const chargeRenames = [
  ["start_date_ms", "started_at_ms"],
  ["end_date_ms", "ended_at_ms"],
  ["start_ideal_range_km", "start_ideal_battery_range_km"],
  ["end_ideal_range_km", "end_ideal_battery_range_km"],
  ["start_rated_range_km", "start_battery_range_km"],
  ["end_rated_range_km", "end_battery_range_km"],
  ["geofence", "geofence_name"],
  ["billing_type", "tariff_type"],
  ["cost_per_unit", "tariff_rate"],
  ["session_fee", "tariff_session_fee"],
  ["charge_energy_added", "charge_energy_added_kwh"],
  ["outside_temp_avg", "outside_temp_avg_c"],
] as const;
const positionRenames = [
  ["date_ms", "timestamp_ms"],
  ["speed", "speed_kmh"],
  ["odometer", "odometer_km"],
  ["rated_battery_range_km", "battery_range_km"],
] as const;
const sampleRenames = [
  ["ideal_range_km", "ideal_battery_range_km"],
  ["rated_range_km", "battery_range_km"],
] as const;

// C6 R47: 28 current renames; the two additions; the model alias is the one type exception.
type CurrentRenamesAgree = Check<Same<Current, (typeof currentRenames)[number]>>;
type CarRenamesAgree = Check<Same<Car, (typeof carRenames)[number]>>;
type SettingsRenamesAgree = Check<Same<Settings, (typeof settingsRenames)[number]>>;
// C6 R58: the 33 history renames.
type DriveRenamesAgree = Check<Same<Drive, (typeof driveRenames)[number]>>;
type ChargeRenamesAgree = Check<Same<Charge, (typeof chargeRenames)[number]>>;
type PositionRenamesAgree = Check<Same<Position, (typeof positionRenames)[number]>>;
type SampleRenamesAgree = Check<Same<ChargeSample, (typeof sampleRenames)[number]>>;
// A tuple, so that one open type makes the whole witness fail (an intersection with a `false`
// member would reduce to `never`, which satisfies any `extends`).
type NoOpenSignatures = Check<
  [
    Closed<Current>,
    Closed<Car>,
    Closed<Settings>,
    Closed<Drive>,
    Closed<Charge>,
    Closed<Position>,
    Closed<ChargeSample>,
  ] extends [true, true, true, true, true, true, true]
    ? true
    : false
>;

type CurrentTypes = {
  activity_state: string | null;
  state_since_ms: number | null;
  processing_healthy: boolean | null;
  geofence_name: string | null;
  model_code: string | null;
  model_name: string | null;
  model_year: number | null;
  software_version: string | null;
  software_update_available: boolean | null;
  software_update_status: string | null;
  software_update_version: string | null;
  software_update_download_perc: number | null;
  software_update_install_perc: number | null;
  speed_kmh: number | null;
  odometer_km: number | null;
  battery_range_km: number | null;
  active_route_km_to_arrival: number | null;
  active_route_miles_to_arrival: number | null;
  charge_cable_connected: boolean | null;
  any_door_open: boolean | null;
  any_window_open: boolean | null;
  door_driver_front_open: boolean | null;
  door_driver_rear_open: boolean | null;
  door_passenger_front_open: boolean | null;
  door_passenger_rear_open: boolean | null;
  window_driver_front_open: boolean | null;
  window_driver_rear_open: boolean | null;
  window_passenger_front_open: boolean | null;
  window_passenger_rear_open: boolean | null;
  front_trunk_open: boolean | null;
  rear_trunk_open: boolean | null;
  car: Car | null;
};
type CarTypes = {
  model: string;
  model_code: string | null;
  model_name: string | null;
  model_year: number | null;
  variant_name: string | null;
  api_vehicle_id: number | null;
  stream_vehicle_id: number | null;
  efficiency_wh_per_km: number | null;
};
type SettingsTypes = {
  collection_enabled: boolean;
  streaming_enabled: boolean;
  sleep_window_min: number;
  sleep_after_idle_min: number;
  sleep_window_min_resolved: boolean;
  sleep_requires_locked: boolean;
  supercharging_free: boolean;
  battery_lfp: boolean;
};
type DriveTypes = {
  started_at_ms: number;
  ended_at_ms: number;
  start_ideal_battery_range_km: number | null;
  end_ideal_battery_range_km: number | null;
  start_battery_range_km: number | null;
  end_battery_range_km: number | null;
  start_geofence_name: string | null;
  end_geofence_name: string | null;
  speed_max_kmh: number | null;
  power_max_kw: number | null;
  power_min_kw: number | null;
  outside_temp_avg_c: number | null;
  inside_temp_avg_c: number | null;
  ascent_m: number | null;
  descent_m: number | null;
  efficiency: number | null; // kept (C6 R58 (c)): its unit is not stated
  duration_min: number | null;
};
type ChargeTypes = {
  started_at_ms: number;
  ended_at_ms: number;
  start_ideal_battery_range_km: number | null;
  end_ideal_battery_range_km: number | null;
  start_battery_range_km: number | null;
  end_battery_range_km: number | null;
  geofence_name: string | null;
  tariff_type: string | null;
  tariff_rate: number | null;
  tariff_session_fee: number | null;
  charge_energy_added_kwh: number | null;
  outside_temp_avg_c: number | null;
  cost: number | null;
};
type PositionTypes = {
  timestamp_ms: number;
  speed_kmh: number | null;
  odometer_km: number | null;
  battery_range_km: number | null;
  power: number | null;
};
type SampleTypes = {
  ideal_battery_range_km: number | null;
  battery_range_km: number | null;
  charge_cable: string | null; // kept: the cable text passes through
  charge_energy_added_kwh: number | null;
};
type CurrentTyped = Check<Typed<Current, CurrentTypes>>;
type CarTyped = Check<Typed<Car, CarTypes>>;
type SettingsTyped = Check<Typed<Settings, SettingsTypes>>;
type DriveTyped = Check<Typed<Drive, DriveTypes>>;
type ChargeTyped = Check<Typed<Charge, ChargeTypes>>;
type PositionTyped = Check<Typed<Position, PositionTypes>>;
type SampleTyped = Check<Typed<ChargeSample, SampleTypes>>;

// Each witness type above is `true` or the file does not compile; this tuple makes every one of
// them used, and the test below checks that it holds all of them.
const witnesses: readonly [
  CurrentRenamesAgree,
  CarRenamesAgree,
  SettingsRenamesAgree,
  DriveRenamesAgree,
  ChargeRenamesAgree,
  PositionRenamesAgree,
  SampleRenamesAgree,
  NoOpenSignatures,
  CurrentTyped,
  CarTyped,
  SettingsTyped,
  DriveTyped,
  ChargeTyped,
  PositionTyped,
  SampleTyped,
] = [true, true, true, true, true, true, true, true, true, true, true, true, true, true, true];

// ------------------------------------------------------------------ typed fixtures
// Every key of each generated type, once with distinct non-null values and once with every
// nullable value null. An alias holds its replacement's value but for the two C6 R50 exceptions
// (`active_route_miles_to_arrival` and `car.model`).

const currentDistinct: Current = {
  active_route_destination: "v-active_route_destination",
  active_route_energy_at_arrival: 2.25,
  active_route_km_to_arrival: 20.12,
  active_route_latitude: 4.25,
  active_route_longitude: 5.25,
  active_route_miles_to_arrival: 12.5,
  active_route_minutes_to_arrival: 6.25,
  active_route_traffic_minutes_delay: 7.25,
  activity_state: "v-activity_state",
  any_door_open: false,
  any_window_open: true,
  battery_level: 1077,
  battery_range_km: 12.25,
  car: {
    api_vehicle_id: 1007,
    efficiency_wh_per_km: 2.25,
    exterior_color: "v-exterior_color",
    firmware_version: "v-firmware_version",
    id: 1,
    model: "Model 3 Long Range",
    model_code: "3",
    model_name: "v-model_name",
    model_year: 1063,
    name: "Synthetic car",
    spoiler_type: "v-spoiler_type",
    stream_vehicle_id: 1091,
    trim_badging: "v-trim_badging",
    variant_name: "v-variant_name",
    vin: "v-vin",
    wheel_type: "v-wheel_type",
    marketing_name: "v-variant_name",
    source_eid: 1007,
    source_vid: 1091,
    settings: {
      battery_lfp: false,
      collection_enabled: true,
      sleep_after_idle_min: 1021,
      sleep_requires_locked: true,
      sleep_window_min: 1035,
      sleep_window_min_resolved: true,
      streaming_enabled: false,
      supercharging_free: true,
      enabled: true,
      free_supercharging: true,
      lfp_battery: false,
      req_not_unlocked: true,
      suspend_after_idle_min: 1021,
      suspend_min: 1035,
      suspend_min_resolved: true,
      use_streaming_api: false,
    },
  },
  center_display_state: 1098,
  charge_cable_connected: false,
  charge_current_request: 1112,
  charge_current_request_max: 1119,
  charge_energy_added: 18.25,
  charge_limit_soc: 1133,
  charge_port_door_open: true,
  charger_actual_current: 21.25,
  charger_phases: 1154,
  charger_power: 23.25,
  charger_voltage: 24.25,
  charging_state: "v-charging_state",
  climate_keeper_mode: "v-climate_keeper_mode",
  display_name: "v-display_name",
  door_driver_front_open: true,
  door_driver_rear_open: false,
  door_passenger_front_open: true,
  door_passenger_rear_open: false,
  doors_open: false,
  download_perc: 1420,
  driver_front_door_open: true,
  driver_front_window_open: true,
  driver_rear_door_open: false,
  driver_rear_window_open: false,
  elevation: 32.25,
  est_battery_range_km: 33.25,
  exterior_color: "v-exterior_color",
  front_trunk_open: false,
  frunk_open: false,
  geofence: "v-geofence_name",
  geofence_name: "v-geofence_name",
  heading: 37.25,
  healthy: false,
  ideal_battery_range_km: 38.25,
  inside_temp: 39.25,
  install_perc: 1427,
  is_climate_on: true,
  is_preconditioning: false,
  is_user_present: true,
  latitude: 43.25,
  locked: true,
  longitude: 45.25,
  model: "3",
  model_code: "3",
  model_name: "v-model_name",
  model_year: 1336,
  observed_at_ms: 1343,
  odometer: 50.25,
  odometer_km: 50.25,
  outside_temp: 51.25,
  passenger_front_door_open: true,
  passenger_front_window_open: true,
  passenger_rear_door_open: false,
  passenger_rear_window_open: false,
  plugged_in: false,
  power: 52.25,
  processing_healthy: false,
  rated_battery_range_km: 12.25,
  rear_trunk_open: true,
  scheduled_charging_start_time: 1385,
  sentry_mode: true,
  service_mode: false,
  shift_state: "v-shift_state",
  since: 1469,
  software_update_available: false,
  software_update_download_perc: 1420,
  software_update_install_perc: 1427,
  software_update_status: "v-software_update_status",
  software_update_version: "v-software_update_version",
  software_version: "v-software_version",
  speed: 1455,
  speed_kmh: 1455,
  spoiler_type: "v-spoiler_type",
  state: "v-activity_state",
  state_since_ms: 1469,
  sun_roof_installed: true,
  sun_roof_percent_open: 1483,
  sun_roof_state: "v-sun_roof_state",
  time_to_full_charge: 71.25,
  tpms_pressure_fl: 72.25,
  tpms_pressure_fr: 73.25,
  tpms_pressure_rl: 74.25,
  tpms_pressure_rr: 75.25,
  tpms_soft_warning_fl: true,
  tpms_soft_warning_fr: false,
  tpms_soft_warning_rl: true,
  tpms_soft_warning_rr: false,
  trim_badging: "v-trim_badging",
  trunk_open: true,
  update_available: false,
  update_status: "v-software_update_status",
  update_version: "v-software_update_version",
  usable_battery_level: 1567,
  vehicle_id: "11111111-1111-4111-8111-111111111111",
  version: "v-software_version",
  wheel_type: "v-wheel_type",
  window_driver_front_open: true,
  window_driver_rear_open: false,
  window_passenger_front_open: true,
  window_passenger_rear_open: false,
  windows_open: true,
};

const currentNull: Current = {
  active_route_destination: null,
  active_route_energy_at_arrival: null,
  active_route_km_to_arrival: null,
  active_route_latitude: null,
  active_route_longitude: null,
  active_route_miles_to_arrival: null,
  active_route_minutes_to_arrival: null,
  active_route_traffic_minutes_delay: null,
  activity_state: null,
  any_door_open: null,
  any_window_open: null,
  battery_level: null,
  battery_range_km: null,
  car: {
    api_vehicle_id: null,
    efficiency_wh_per_km: null,
    exterior_color: null,
    firmware_version: null,
    id: 1,
    model: "Model 3 Long Range",
    model_code: null,
    model_name: null,
    model_year: null,
    name: "Synthetic car",
    spoiler_type: null,
    stream_vehicle_id: null,
    trim_badging: null,
    variant_name: null,
    vin: null,
    wheel_type: null,
    marketing_name: null,
    source_eid: null,
    source_vid: null,
    settings: {
      battery_lfp: false,
      collection_enabled: true,
      sleep_after_idle_min: 1021,
      sleep_requires_locked: true,
      sleep_window_min: 1035,
      sleep_window_min_resolved: true,
      streaming_enabled: false,
      supercharging_free: true,
      enabled: true,
      free_supercharging: true,
      lfp_battery: false,
      req_not_unlocked: true,
      suspend_after_idle_min: 1021,
      suspend_min: 1035,
      suspend_min_resolved: true,
      use_streaming_api: false,
    },
  },
  center_display_state: null,
  charge_cable_connected: null,
  charge_current_request: null,
  charge_current_request_max: null,
  charge_energy_added: null,
  charge_limit_soc: null,
  charge_port_door_open: null,
  charger_actual_current: null,
  charger_phases: null,
  charger_power: null,
  charger_voltage: null,
  charging_state: null,
  climate_keeper_mode: null,
  display_name: null,
  door_driver_front_open: null,
  door_driver_rear_open: null,
  door_passenger_front_open: null,
  door_passenger_rear_open: null,
  doors_open: null,
  download_perc: null,
  driver_front_door_open: null,
  driver_front_window_open: null,
  driver_rear_door_open: null,
  driver_rear_window_open: null,
  elevation: null,
  est_battery_range_km: null,
  exterior_color: null,
  front_trunk_open: null,
  frunk_open: null,
  geofence: null,
  geofence_name: null,
  heading: null,
  healthy: null,
  ideal_battery_range_km: null,
  inside_temp: null,
  install_perc: null,
  is_climate_on: null,
  is_preconditioning: null,
  is_user_present: null,
  latitude: null,
  locked: null,
  longitude: null,
  model: null,
  model_code: null,
  model_name: null,
  model_year: null,
  observed_at_ms: null,
  odometer: null,
  odometer_km: null,
  outside_temp: null,
  passenger_front_door_open: null,
  passenger_front_window_open: null,
  passenger_rear_door_open: null,
  passenger_rear_window_open: null,
  plugged_in: null,
  power: null,
  processing_healthy: null,
  rated_battery_range_km: null,
  rear_trunk_open: null,
  scheduled_charging_start_time: null,
  sentry_mode: null,
  service_mode: null,
  shift_state: null,
  since: null,
  software_update_available: null,
  software_update_download_perc: null,
  software_update_install_perc: null,
  software_update_status: null,
  software_update_version: null,
  software_version: null,
  speed: null,
  speed_kmh: null,
  spoiler_type: null,
  state: null,
  state_since_ms: null,
  sun_roof_installed: null,
  sun_roof_percent_open: null,
  sun_roof_state: null,
  time_to_full_charge: null,
  tpms_pressure_fl: null,
  tpms_pressure_fr: null,
  tpms_pressure_rl: null,
  tpms_pressure_rr: null,
  tpms_soft_warning_fl: null,
  tpms_soft_warning_fr: null,
  tpms_soft_warning_rl: null,
  tpms_soft_warning_rr: null,
  trim_badging: null,
  trunk_open: null,
  update_available: null,
  update_status: null,
  update_version: null,
  usable_battery_level: null,
  vehicle_id: "11111111-1111-4111-8111-111111111111",
  version: null,
  wheel_type: null,
  window_driver_front_open: null,
  window_driver_rear_open: null,
  window_passenger_front_open: null,
  window_passenger_rear_open: null,
  windows_open: null,
};

const driveDistinct: Drive = {
  ascent: 1007,
  ascent_m: 1007,
  descent: 1014,
  descent_m: 1014,
  distance_km: 3.25,
  duration_min: 1028,
  efficiency: 5.25,
  end_address: "v-end_address",
  end_battery_range_km: 8.25,
  end_date_ms: 1098,
  end_geofence: "v-end_geofence_name",
  end_geofence_name: "v-end_geofence_name",
  end_ideal_battery_range_km: 10.25,
  end_ideal_range_km: 10.25,
  end_latitude: 11.25,
  end_longitude: 12.25,
  end_rated_range_km: 8.25,
  end_soc: 1091,
  ended_at_ms: 1098,
  id: 1105,
  inside_temp_avg: 16.25,
  inside_temp_avg_c: 16.25,
  outside_temp_avg: 17.25,
  outside_temp_avg_c: 17.25,
  power_max: 18.25,
  power_max_kw: 18.25,
  power_min: 19.25,
  power_min_kw: 19.25,
  speed_max: 1140,
  speed_max_kmh: 1140,
  start_address: "v-start_address",
  start_battery_range_km: 23.25,
  start_date_ms: 1203,
  start_geofence: "v-start_geofence_name",
  start_geofence_name: "v-start_geofence_name",
  start_ideal_battery_range_km: 25.25,
  start_ideal_range_km: 25.25,
  start_latitude: 26.25,
  start_longitude: 27.25,
  start_rated_range_km: 23.25,
  start_soc: 1196,
  started_at_ms: 1203,
  vehicle_id: "11111111-1111-4111-8111-111111111111",
};

const driveNull: Drive = {
  ascent: null,
  ascent_m: null,
  descent: null,
  descent_m: null,
  distance_km: null,
  duration_min: null,
  efficiency: null,
  end_address: null,
  end_battery_range_km: null,
  end_date_ms: 1098,
  end_geofence: null,
  end_geofence_name: null,
  end_ideal_battery_range_km: null,
  end_ideal_range_km: null,
  end_latitude: null,
  end_longitude: null,
  end_rated_range_km: null,
  end_soc: null,
  ended_at_ms: 1098,
  id: 1105,
  inside_temp_avg: null,
  inside_temp_avg_c: null,
  outside_temp_avg: null,
  outside_temp_avg_c: null,
  power_max: null,
  power_max_kw: null,
  power_min: null,
  power_min_kw: null,
  speed_max: null,
  speed_max_kmh: null,
  start_address: null,
  start_battery_range_km: null,
  start_date_ms: 1203,
  start_geofence: null,
  start_geofence_name: null,
  start_ideal_battery_range_km: null,
  start_ideal_range_km: null,
  start_latitude: null,
  start_longitude: null,
  start_rated_range_km: null,
  start_soc: null,
  started_at_ms: 1203,
  vehicle_id: "11111111-1111-4111-8111-111111111111",
};

const chargeDistinct: Charge = {
  address: "v-address",
  billing_type: "v-tariff_type",
  charge_energy_added: 3.25,
  charge_energy_added_kwh: 3.25,
  charge_energy_used_kwh: 4.25,
  charge_rate_km_per_hour: 5.25,
  cost: 6.25,
  cost_per_unit: 25.25,
  duration_min: 1049,
  end_battery_level: 1056,
  end_battery_range_km: 9.25,
  end_date_ms: 1077,
  end_ideal_battery_range_km: 10.25,
  end_ideal_range_km: 10.25,
  end_rated_range_km: 9.25,
  ended_at_ms: 1077,
  fast_charger_type: "v-fast_charger_type",
  geofence: "v-geofence_name",
  geofence_name: "v-geofence_name",
  id: 1098,
  is_dc: false,
  location_name: "v-location_name",
  max_charger_power_kw: 17.25,
  outside_temp_avg: 18.25,
  outside_temp_avg_c: 18.25,
  session_fee: 26.25,
  start_battery_level: 1133,
  start_battery_range_km: 20.25,
  start_date_ms: 1168,
  start_ideal_battery_range_km: 21.25,
  start_ideal_range_km: 21.25,
  start_latitude: 22.25,
  start_longitude: 23.25,
  start_rated_range_km: 20.25,
  started_at_ms: 1168,
  tariff_rate: 25.25,
  tariff_session_fee: 26.25,
  tariff_type: "v-tariff_type",
  vehicle_id: "11111111-1111-4111-8111-111111111111",
};

const chargeNull: Charge = {
  address: null,
  billing_type: null,
  charge_energy_added: null,
  charge_energy_added_kwh: null,
  charge_energy_used_kwh: null,
  charge_rate_km_per_hour: null,
  cost: null,
  cost_per_unit: null,
  duration_min: null,
  end_battery_level: null,
  end_battery_range_km: null,
  end_date_ms: 1077,
  end_ideal_battery_range_km: null,
  end_ideal_range_km: null,
  end_rated_range_km: null,
  ended_at_ms: 1077,
  fast_charger_type: null,
  geofence: null,
  geofence_name: null,
  id: 1098,
  is_dc: null,
  location_name: null,
  max_charger_power_kw: null,
  outside_temp_avg: null,
  outside_temp_avg_c: null,
  session_fee: null,
  start_battery_level: null,
  start_battery_range_km: null,
  start_date_ms: 1168,
  start_ideal_battery_range_km: null,
  start_ideal_range_km: null,
  start_latitude: null,
  start_longitude: null,
  start_rated_range_km: null,
  started_at_ms: 1168,
  tariff_rate: null,
  tariff_session_fee: null,
  tariff_type: null,
  vehicle_id: "11111111-1111-4111-8111-111111111111",
};

const positionDistinct: Position = {
  battery_heater: false,
  battery_heater_on: true,
  battery_level: 1021,
  battery_range_km: 4.25,
  date_ms: 1154,
  drive_id: 1035,
  driver_temp_setting: 6.25,
  elevation: 1049,
  elevation_provenance: { source: "vehicle", dataset: null, dataset_version: null, tile: null },
  est_battery_range_km: 9.25,
  fan_status: 1070,
  id: 1077,
  ideal_battery_range_km: 12.25,
  inside_temp: 13.25,
  is_climate_on: true,
  latitude: 15.25,
  longitude: 16.25,
  odometer: 17.25,
  odometer_km: 17.25,
  outside_temp: 18.25,
  passenger_temp_setting: 19.25,
  power: 20.25,
  rated_battery_range_km: 4.25,
  speed: 1147,
  speed_kmh: 1147,
  timestamp_ms: 1154,
  tpms_pressure_fl: 23.25,
  tpms_pressure_fr: 24.25,
  tpms_pressure_rl: 25.25,
  tpms_pressure_rr: 26.25,
  usable_battery_level: 1189,
};

const positionNull: Position = {
  battery_heater: null,
  battery_heater_on: null,
  battery_level: null,
  battery_range_km: null,
  date_ms: 1154,
  drive_id: 1035,
  driver_temp_setting: null,
  elevation: null,
  elevation_provenance: { source: "vehicle", dataset: null, dataset_version: null, tile: null },
  est_battery_range_km: null,
  fan_status: null,
  id: 1077,
  ideal_battery_range_km: null,
  inside_temp: null,
  is_climate_on: null,
  latitude: null,
  longitude: null,
  odometer: null,
  odometer_km: null,
  outside_temp: null,
  passenger_temp_setting: null,
  power: null,
  rated_battery_range_km: null,
  speed: null,
  speed_kmh: null,
  timestamp_ms: 1154,
  tpms_pressure_fl: null,
  tpms_pressure_fr: null,
  tpms_pressure_rl: null,
  tpms_pressure_rr: null,
  usable_battery_level: null,
};

const sampleDistinct: ChargeSample = {
  battery_heater_on: false,
  battery_level: 1014,
  battery_range_km: 3.25,
  charge_cable: "v-charge_cable",
  charge_energy_added_kwh: 5.25,
  charge_id: 1042,
  charger_actual_current: 7.25,
  charger_phases: 1056,
  charger_pilot_current: 9.25,
  charger_power_kw: 10.25,
  charger_voltage: 11.25,
  fast_charger_brand: "v-fast_charger_brand",
  fast_charger_present: false,
  fast_charger_type: "v-fast_charger_type",
  id: 1105,
  ideal_battery_range_km: 16.25,
  ideal_range_km: 16.25,
  outside_temp_c: 17.25,
  rated_range_km: 3.25,
  timestamp_ms: 1126,
  usable_battery_level: 1133,
};

const sampleNull: ChargeSample = {
  battery_heater_on: null,
  battery_level: null,
  battery_range_km: null,
  charge_cable: null,
  charge_energy_added_kwh: null,
  charge_id: 1042,
  charger_actual_current: null,
  charger_phases: null,
  charger_pilot_current: null,
  charger_power_kw: null,
  charger_voltage: null,
  fast_charger_brand: null,
  fast_charger_present: null,
  fast_charger_type: null,
  id: 1105,
  ideal_battery_range_km: null,
  ideal_range_km: null,
  outside_temp_c: null,
  rated_range_km: null,
  timestamp_ms: 1126,
  usable_battery_level: null,
};

// ------------------------------------------------------------------ runtime helpers

type Json = Record<string, unknown>;

function camel(key: string): string {
  return key.replace(/_([a-z])/gu, (_match, letter: string) => letter.toUpperCase());
}

function sameValue(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function aliasProblems(
  object: Json,
  pairs: ReadonlyArray<readonly [string, string]>,
  exceptions: readonly string[] = [],
): string[] {
  const problems: string[] = [];
  for (const [old, replacement] of pairs) {
    if (!(old in object) || !(replacement in object)) problems.push(`${old}/${replacement} absent`);
    else if (!exceptions.includes(old) && !sameValue(object[old], object[replacement]))
      problems.push(`${old} differs from ${replacement}`);
  }
  return problems;
}

// A decoded (camelized) object holds both spellings with the same value.
function camelProblems(
  object: Json,
  pairs: ReadonlyArray<readonly [string, string]>,
  exceptions: readonly string[] = [],
): string[] {
  return aliasProblems(
    object,
    pairs.map(([old, replacement]) => [camel(old), camel(replacement)] as const),
    exceptions.map(camel),
  );
}

function asPage(item: unknown): string {
  return JSON.stringify({ items: [item], next_cursor: null }, null, 2);
}

// ------------------------------------------------------------------ integer range cases

interface IntegerCase {
  readonly field: string; // the new wire key
  readonly alias: string | null; // its retained 1.2.0 alias, null for an addition
  readonly validator: "HubCurrent" | "HubDrives";
  readonly area: "current" | "car" | "settings" | "drive";
}

// Every integer key that C6 adds and the nine retained validators reach (the work order's
// table: `model_year` is one entry covering its two declared locations, here two cases).
const integerCases: readonly IntegerCase[] = [
  { field: "state_since_ms", alias: "since", validator: "HubCurrent", area: "current" },
  {
    field: "software_update_download_perc",
    alias: "download_perc",
    validator: "HubCurrent",
    area: "current",
  },
  {
    field: "software_update_install_perc",
    alias: "install_perc",
    validator: "HubCurrent",
    area: "current",
  },
  { field: "speed_kmh", alias: "speed", validator: "HubCurrent", area: "current" },
  { field: "model_year", alias: null, validator: "HubCurrent", area: "current" },
  { field: "api_vehicle_id", alias: "source_eid", validator: "HubCurrent", area: "car" },
  { field: "stream_vehicle_id", alias: "source_vid", validator: "HubCurrent", area: "car" },
  { field: "model_year", alias: null, validator: "HubCurrent", area: "car" },
  { field: "sleep_window_min", alias: "suspend_min", validator: "HubCurrent", area: "settings" },
  {
    field: "sleep_after_idle_min",
    alias: "suspend_after_idle_min",
    validator: "HubCurrent",
    area: "settings",
  },
  { field: "started_at_ms", alias: "start_date_ms", validator: "HubDrives", area: "drive" },
  { field: "ended_at_ms", alias: "end_date_ms", validator: "HubDrives", area: "drive" },
  { field: "speed_max_kmh", alias: "speed_max", validator: "HubDrives", area: "drive" },
  { field: "ascent_m", alias: "ascent", validator: "HubDrives", area: "drive" },
  { field: "descent_m", alias: "descent", validator: "HubDrives", area: "drive" },
];
const indentOf = { current: 2, car: 4, settings: 6, drive: 6 } as const;
const aboveMaximum = "9223372036854775808"; // exactly representable, one above the signed-i64 maximum
const belowMinimum = "-9223372036854777856"; // exactly representable, below the signed-i64 minimum
const roundsUp = "9007199254740993"; // not representable: JSON.parse would round it
const fractional = "5.00000000000000001"; // a fraction that rounds to an integer
const inRangeControls = ["9223372036854774784", "-9223372036854775808"];

function baseBytes(area: IntegerCase["area"]): string {
  return area === "drive" ? asPage(driveNull) : JSON.stringify(currentNull, null, 2);
}

// The raw token goes into the JSON text itself; no number is built and stringified.
function withToken(area: IntegerCase["area"], key: string, token: string): string {
  const pattern = new RegExp(`(\\n {${indentOf[area]}}"${key}": )[^,\\n]+`, "u");
  const bytes = baseBytes(area);
  if (!pattern.test(bytes)) throw new Error(`no ${area}.${key} member in the base body`);
  return bytes.replace(pattern, (_match, head: string) => `${head}${token}`);
}

function decoded(testCase: IntegerCase, bytes: string): Json {
  const value =
    testCase.validator === "HubCurrent"
      ? decodeHubJson(bytes, validateHubCurrent, "HubCurrent")
      : decodeHubJson(bytes, validateHubDrives, "HubDrives");
  return value as unknown as Json;
}

function target(testCase: IntegerCase, value: Json): Json {
  switch (testCase.area) {
    case "current":
      return value;
    case "car":
      return value.car as Json;
    case "settings":
      return (value.car as Json).settings as Json;
    case "drive":
      return (value.items as Json[])[0] as Json;
  }
}

function label(testCase: IntegerCase, token: string): string {
  return `${testCase.area}.${testCase.field} = ${token}`;
}

// The case passes only when the production decoder refuses with the manual integer guard's own
// identity (`<validator>.integer`): the generated validator names the bare validator instead.
function refusedByGuard(testCase: IntegerCase, key: string, token: string): void {
  const name = `${testCase.area}.${key} = ${token}`;
  try {
    decoded(testCase, withToken(testCase.area, key, token));
  } catch (error) {
    expect(error instanceof ProtocolValidationError, `${name}: a validation error`).toBe(true);
    expect((error as ProtocolValidationError).validator, name).toBe(
      `${testCase.validator}.integer`,
    );
    return;
  }
  throw new Error(`${name}: accepted`);
}

describe("C6 generated client aliases", () => {
  it("c6_generated_current_car_settings_types", () => {
    // The witnesses above are compile-time: `npm run typecheck` fails when any pair, type or
    // declared key is wrong. This runtime half checks the fixtures built from the same types.
    for (const fixture of [currentDistinct, currentNull]) {
      expect(Object.keys(fixture)).toHaveLength(115);
      expect(Object.keys(fixture.car ?? {})).toHaveLength(20);
      expect(Object.keys(fixture.car?.settings ?? {})).toHaveLength(16);
      for (const [old, replacement] of currentRenames) {
        expect(old in fixture && replacement in fixture, `${old}/${replacement}`).toBe(true);
      }
    }
    expect(witnesses).toHaveLength(15);
    expect(witnesses.every((witness) => witness)).toBe(true);
    expect(currentRenames).toHaveLength(28);
    expect(carRenames).toHaveLength(3);
    expect(carModelPair).toEqual(["model", "model_code"]);
    expect(currentDistinct.car?.model).not.toBe(currentDistinct.car?.model_code);
    expect(settingsRenames).toHaveLength(8);
    expect("model_name" in currentDistinct && "model_year" in currentDistinct).toBe(true);
    expect("source_model_text" in (currentDistinct.car ?? {})).toBe(false);
  });

  it("c6_generated_history_33_types", () => {
    // 15 + 12 + 4 + 2 renames, every key of each generated type present in its fixtures.
    expect(
      driveRenames.length + chargeRenames.length + positionRenames.length + sampleRenames.length,
    ).toBe(33);
    for (const [fixture, count] of [
      [driveDistinct, 43],
      [driveNull, 43],
      [chargeDistinct, 39],
      [chargeNull, 39],
      [positionDistinct, 31],
      [positionNull, 31],
      [sampleDistinct, 21],
      [sampleNull, 21],
    ] as const) {
      expect(Object.keys(fixture)).toHaveLength(count);
    }
    expect(driveDistinct.efficiency).not.toBeNull(); // kept under its own name
    expect(driveNull.efficiency).toBeNull();
    expect("efficiency" in driveDistinct && "charge_cable" in sampleDistinct).toBe(true);
  });

  it("c6_generated_alias_runtime_values", () => {
    // Both rename sets through the generated validators and the production decoder.
    for (const fixture of [currentDistinct, currentNull]) {
      const value = decodeHubJson(
        JSON.stringify(fixture),
        validateHubCurrent,
        "HubCurrent",
      ) as unknown as Json;
      expect(camelProblems(value, currentRenames, ["active_route_miles_to_arrival"])).toEqual([]);
      const car = value.car as Json;
      expect(camelProblems(car, carRenames)).toEqual([]);
      expect(camelProblems(car.settings as Json, settingsRenames)).toEqual([]);
      // R50's two exceptions: the alias keeps what 1.2.0 served.
      expect(car.model).toBe("Model 3 Long Range");
      expect(value.activeRouteMilesToArrival).toBe(fixture.active_route_miles_to_arrival);
      expect(value.activeRouteKmToArrival).toBe(fixture.active_route_km_to_arrival);
      expect(value.activeRouteMilesToArrival === value.activeRouteKmToArrival).toBe(
        fixture.active_route_km_to_arrival === null,
      );
    }
    for (const fixture of [driveDistinct, driveNull]) {
      const page = decodeHubJson(asPage(fixture), validateHubDrives, "HubDrives");
      expect(camelProblems(page.items[0] as unknown as Json, driveRenames)).toEqual([]);
    }
    for (const [fixture, pairs] of [
      [driveDistinct, driveRenames],
      [driveNull, driveRenames],
      [chargeDistinct, chargeRenames],
      [chargeNull, chargeRenames],
      [positionDistinct, positionRenames],
      [positionNull, positionRenames],
      [sampleDistinct, sampleRenames],
      [sampleNull, sampleRenames],
    ] as const) {
      expect(aliasProblems(fixture as unknown as Json, pairs)).toEqual([]);
    }

    // The signed-i64 guard of every new integer key, through the production decoder.
    let refusals = 0;
    for (const testCase of integerCases) {
      // The otherwise valid base body is accepted, and so is each in-range control.
      expect(
        () => decoded(testCase, baseBytes(testCase.area)),
        label(testCase, "base"),
      ).not.toThrow();
      for (const token of inRangeControls) {
        const value = target(
          testCase,
          decoded(testCase, withToken(testCase.area, testCase.field, token)),
        );
        expect(value[camel(testCase.field)], label(testCase, token)).toBe(Number(token));
      }
      for (const token of [aboveMaximum, belowMinimum, roundsUp, fractional]) {
        refusedByGuard(testCase, testCase.field, token);
        refusals += 1;
      }
      // The retained alias keeps its own refusal beside it, and is legal on its own.
      if (testCase.alias !== null) {
        refusedByGuard(testCase, testCase.alias, aboveMaximum);
        refusals += 1;
        const value = target(
          testCase,
          decoded(testCase, withToken(testCase.area, testCase.alias, "7")),
        );
        expect(value[camel(testCase.alias)], label(testCase, "legal alias")).toBe(7);
      }
    }
    expect(refusals).toBe(15 * 4 + 13);

    // Ordinary finite fractional measurements stay accepted beside the integer fields.
    const measured = JSON.stringify(currentNull, null, 2)
      .replace('"inside_temp": null', '"inside_temp": 20.25')
      .replace('"charge_energy_added": null', '"charge_energy_added": 7.5');
    expect(decodeHubJson(measured, validateHubCurrent, "HubCurrent")).toMatchObject({
      insideTemp: 20.25,
      chargeEnergyAdded: 7.5,
    });
    expect(() =>
      decodeHubJson(
        asPage(driveNull).replace('"distance_km": null', '"distance_km": 12.5'),
        validateHubDrives,
        "HubDrives",
      ),
    ).not.toThrow();
  });

  it("c6_generated_profile_identity", async () => {
    const profileRoot = new URL("../../../protocol/profiles/hub-http-v1/1.3.0/", import.meta.url);
    const profile = JSON.parse(
      await readFile(new URL("profile.json", profileRoot), "utf8"),
    ) as Json;
    expect(profile.profile_id).toBe("hub-http-v1@1.3.0");
    expect(profile.contract_version).toBe("1.3.0");

    // The generated validators name the allocated schemas and no other minor.
    const validatorsUrl = new URL("../../src/generated/hub-validators.js", import.meta.url);
    const source = await readFile(validatorsUrl, "utf8");
    for (const name of ["discovery", "errors", "auth", "resources"]) {
      const schema = JSON.parse(
        await readFile(new URL(`${name}.schema.json`, profileRoot), "utf8"),
      ) as Json;
      expect(schema.$id).toBe(`urn:teslatlas:hub-http-v1:1.3.0:${name}`);
    }
    expect(source).not.toContain("urn:teslatlas:hub-http-v1:1.2.0");
    // ... and it names this one: the two schemas the compiler keeps by identity, and each of the nine
    // exported validators accepts the published 1.3.0 example it validates (a generation from
    // another minor, or from no profile, would not).
    expect(source).toContain("urn:teslatlas:hub-http-v1:1.3.0:discovery");
    expect(source).toContain("urn:teslatlas:hub-http-v1:1.3.0:errors");
    const generated = (await import("../../src/generated/hub-validators.js")) as unknown as Record<
      string,
      (value: unknown) => boolean
    >;
    const exampleOf: Record<string, string> = {
      validateHubDiscovery: "discovery.json",
      validateHubError: "error-drive-not-found.json",
      validateHubClaim: "claim.json",
      validateHubInvitation: "invitation.json",
      validateHubVehicles: "vehicles.json",
      validateHubCurrent: "current.json",
      validateHubDrives: "drives.json",
      validateHubHealth: "health.json",
      validateHubReady: "ready.json",
    };
    expect(Object.keys(exampleOf).sort()).toEqual(
      Object.keys(generated)
        .filter((name) => name.startsWith("validateHub"))
        .sort(),
    );
    for (const [validator, file] of Object.entries(exampleOf)) {
      const example = JSON.parse(
        await readFile(new URL(`examples/${file}`, profileRoot), "utf8"),
      ) as unknown;
      const check = generated[validator];
      expect(check?.(example), `${validator} accepts ${file}`).toBe(true);
    }
    // The generated sources were generated from this profile: its deprecated aliases and new keys.
    const protocolSource = await readFile(
      new URL("../../src/generated/hub-protocol.ts", import.meta.url),
      "utf8",
    );
    const aliases = profile.deprecated_aliases as Record<string, Record<string, string>>;
    for (const item of ["current", "drive", "charge", "position", "charge_sample"]) {
      for (const [old, replacement] of Object.entries(aliases[item] ?? {})) {
        expect(protocolSource, `${item}.${replacement}`).toContain(`${replacement}: `);
        expect(protocolSource, `${item}.${old}`).toContain(`Deprecated alias of ${replacement}`);
      }
    }

    // The declaration file exports exactly the validators the module exports.
    const exported = Object.keys(await import("../../src/generated/hub-validators.js")).sort();
    const declared = [
      ...(
        await readFile(new URL("../../src/generated/hub-validators.d.ts", import.meta.url), "utf8")
      ).matchAll(/^export const (\w+): GeneratedHubValidator;$/gmu),
    ]
      .map((match) => match[1])
      .sort();
    expect(declared).toEqual(exported);
    expect(declared).toHaveLength(9);
  });
});
