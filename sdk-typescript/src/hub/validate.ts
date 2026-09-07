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
  rejectUnsafeIntegers(source, validatorName);
  let wire: unknown;
  try {
    wire = JSON.parse(source);
  } catch {
    throw new ProtocolValidationError(validatorName);
  }
  if (!validator(wire)) throw new ProtocolValidationError(validatorName);
  return camelize(wire) as CamelizeKeys<T>;
}

function rejectUnsafeIntegers(source: string, validatorName: string): void {
  let index = 0;
  while (index < source.length) {
    const character = source[index];
    if (character === '"') {
      index += 1;
      while (index < source.length) {
        if (source[index] === "\\") {
          index += 2;
        } else if (source[index] === '"') {
          index += 1;
          break;
        } else {
          index += 1;
        }
      }
      continue;
    }
    if (character === "-" || (character !== undefined && /[0-9]/u.test(character))) {
      const match = source
        .slice(index)
        .match(/^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/u);
      if (match !== null) {
        const value = Number(match[0]);
        if (Number.isInteger(value) && !Number.isSafeInteger(value)) {
          throw new ProtocolValidationError(`${validatorName}.integer`);
        }
        index += match[0].length;
        continue;
      }
    }
    index += 1;
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
