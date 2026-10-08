import { ProtocolValidationError } from "../core/errors.js";
import {
  validateHubClaim as generatedValidateHubClaim,
  validateHubCurrent as generatedValidateHubCurrent,
  validateHubDiscovery as generatedValidateHubDiscovery,
  validateHubDrives as generatedValidateHubDrives,
  validateHubError as generatedValidateHubError,
  validateHubHealth as generatedValidateHubHealth,
  validateHubInvitation as generatedValidateHubInvitation,
  validateHubReady as generatedValidateHubReady,
  validateHubVehicles as generatedValidateHubVehicles,
} from "../generated/hub-validators.js";
import { isExactJsonInteger, parseProtocolJson } from "../http/bounded-json.js";
import type {
  CamelizeKeys,
  HubClaim,
  HubCurrent,
  HubDiscovery,
  HubDrives,
  HubErrorEnvelope,
  HubHealth,
  HubInvitation,
  HubReadiness,
  HubVehicles,
} from "./models.js";

export type HubValidator<T> = ((value: unknown) => boolean) & {
  readonly errors?: unknown;
  readonly __validatedType?: T;
};

export const validateHubDiscovery = generatedValidateHubDiscovery as HubValidator<HubDiscovery>;
export const validateHubHealth = generatedValidateHubHealth as HubValidator<HubHealth>;
export const validateHubReady = generatedValidateHubReady as HubValidator<HubReadiness>;
export const validateHubVehicles = generatedValidateHubVehicles as HubValidator<HubVehicles>;
export const validateHubCurrent = generatedValidateHubCurrent as HubValidator<HubCurrent>;
export const validateHubDrives = generatedValidateHubDrives as HubValidator<HubDrives>;
export const validateHubClaim = generatedValidateHubClaim as HubValidator<HubClaim>;
export const validateHubInvitation = generatedValidateHubInvitation as HubValidator<HubInvitation>;
export const validateHubError = generatedValidateHubError as HubValidator<HubErrorEnvelope>;

export function decodeHubJson<T>(
  source: string,
  validator: HubValidator<T>,
  validatorName: string,
): CamelizeKeys<T> {
  const wire = parseProtocolJson(source, validatorName);
  rejectInexactIntegerFields(wire, validatorName);
  if (!validator(wire)) throw new ProtocolValidationError(validatorName);
  return camelize(wire) as CamelizeKeys<T>;
}

// Integer-valued fields in the frozen hub-http-v1@1.0.0 resource/auth schemas,
// plus discovery's integral protocol_major const. Other numeric fields remain
// binary64 measurements; this list does not impose an integer rule on them.
const integerFields = new Set([
  "ascent",
  "battery_level",
  "center_display_state",
  "charge_current_request",
  "charge_current_request_max",
  "charge_limit_soc",
  "charger_phases",
  "descent",
  "download_perc",
  "duration_min",
  "end_date_ms",
  "end_soc",
  "expires_at_ms",
  "expiresAtMs",
  "id",
  "install_perc",
  "observed_at_ms",
  "protocol_major",
  "scheduled_charging_start_time",
  "since",
  "source_eid",
  "source_vid",
  "speed",
  "speed_max",
  "start_date_ms",
  "start_soc",
  "sun_roof_percent_open",
  "suspend_after_idle_min",
  "suspend_min",
  "usable_battery_level",
]);

// Hub resource/auth integer fields are signed i64. Check the exact bounds here:
// the schema's i64 maximum rounds upward when its JSON is loaded as a Number.
function rejectInexactIntegerFields(value: unknown, validatorName: string): void {
  const pending: unknown[] = [value];
  while (pending.length > 0) {
    const current = pending.pop();
    if (current === null || typeof current !== "object") continue;
    for (const [key, child] of Object.entries(current)) {
      if (
        integerFields.has(key) &&
        typeof child === "number" &&
        (!isExactJsonInteger(child, current, key) ||
          BigInt(child) < -9_223_372_036_854_775_808n ||
          BigInt(child) > 9_223_372_036_854_775_807n)
      ) {
        throw new ProtocolValidationError(`${validatorName}.integer`);
      }
      pending.push(child);
    }
  }
}

function camelize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(camelize);
  if (value === null || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.entries(value).map(([key, child]) => [
      key.replace(/_([a-z])/gu, (_match, letter: string) => letter.toUpperCase()),
      camelize(child),
    ]),
  );
}
