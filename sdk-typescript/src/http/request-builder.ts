import type { EntityTag } from "../core/opaque-values.js";
import type { IdempotencyKey } from "../commands/idempotency.js";
import { TeslatlasError } from "../core/errors.js";
import type { SupportedProtocolVersion } from "../protocol/negotiation.js";
import type { StrongEntityTag } from "./strong-etag.js";
import { InvalidRequestPathError, type ProtocolRequestInit } from "./fetch-transport.js";

export type ReadOperationName =
  | "discoverHub"
  | "listVehicles"
  | "getVehicleCurrentState"
  | "listVehicleDrives"
  | "getDrive"
  | "listDrivePositions"
  | "listVehicleCharges"
  | "getCharge"
  | "listChargeSamples"
  | "listVehicleStates"
  | "listVehicleUpdates"
  | "listDataQuality"
  | "listVehicleMetadata"
  | "getMetadata"
  | "getCommand";

export interface ReadOperationDescriptor {
  readonly pathTemplate: string;
  readonly queryNames: readonly string[];
  readonly versioned: boolean;
}

export type WriteOperationName =
  | "createMetadata"
  | "replaceMetadata"
  | "deleteMetadata"
  | "createCommand";

export interface WriteOperationDescriptor {
  readonly pathTemplate: string;
  readonly method: "POST" | "PUT" | "DELETE";
  readonly hasJsonBody: boolean;
}

type QueryValue = string | number | readonly string[] | undefined;
type QueryValues = Readonly<Record<string, QueryValue>>;

export interface BuiltReadRequest {
  readonly path: string;
  readonly init: ProtocolRequestInit;
}

export const readOperationDescriptors = {
  discoverHub: descriptor("/.well-known/teslatlas-hub", [], false),
  listVehicles: descriptor("/v1/vehicles", ["cursor", "limit"]),
  getVehicleCurrentState: descriptor("/v1/vehicles/{vehicle_id}/current", []),
  listVehicleDrives: descriptor("/v1/vehicles/{vehicle_id}/drives", [
    "cursor",
    "limit",
    "from",
    "to",
  ]),
  getDrive: descriptor("/v1/drives/{drive_id}", []),
  listDrivePositions: descriptor("/v1/drives/{drive_id}/positions", [
    "cursor",
    "limit",
    "from",
    "to",
  ]),
  listVehicleCharges: descriptor("/v1/vehicles/{vehicle_id}/charges", [
    "cursor",
    "limit",
    "from",
    "to",
  ]),
  getCharge: descriptor("/v1/charges/{charge_id}", []),
  listChargeSamples: descriptor("/v1/charges/{charge_id}/samples", [
    "cursor",
    "limit",
    "from",
    "to",
  ]),
  listVehicleStates: descriptor("/v1/vehicles/{vehicle_id}/states", [
    "cursor",
    "limit",
    "from",
    "to",
  ]),
  listVehicleUpdates: descriptor("/v1/vehicles/{vehicle_id}/updates", [
    "cursor",
    "limit",
    "from",
    "to",
  ]),
  listDataQuality: descriptor("/v1/data-quality", ["cursor", "limit", "from", "to", "vehicle_id"]),
  listVehicleMetadata: descriptor("/v1/vehicles/{vehicle_id}/metadata", [
    "cursor",
    "limit",
    "kind",
  ]),
  getMetadata: descriptor("/v1/metadata/{metadata_id}", []),
  getCommand: descriptor("/v1/commands/{command_id}", []),
} as const satisfies Readonly<Record<ReadOperationName, ReadOperationDescriptor>>;

export const writeOperationDescriptors = {
  createMetadata: writeDescriptor("/v1/vehicles/{vehicle_id}/metadata", "POST", true),
  replaceMetadata: writeDescriptor("/v1/metadata/{metadata_id}", "PUT", true),
  deleteMetadata: writeDescriptor("/v1/metadata/{metadata_id}", "DELETE", false),
  createCommand: writeDescriptor("/v1/commands", "POST", true),
} as const satisfies Readonly<Record<WriteOperationName, WriteOperationDescriptor>>;

export function interpolatePath(
  template: string,
  values: Readonly<Record<string, string>>,
): string {
  const used = new Set<string>();
  const path = template.replace(/\{([^}]+)\}/gu, (_match, name: string) => {
    const value = values[name];
    if (value === undefined) throw new InvalidRequestPathError();
    used.add(name);
    return encodeURIComponent(value);
  });
  if (
    !path.startsWith("/") ||
    path.startsWith("//") ||
    Object.keys(values).some((name) => !used.has(name))
  ) {
    throw new InvalidRequestPathError();
  }
  return path;
}

export function buildReadRequest(
  operation: ReadOperationDescriptor,
  pathValues: Readonly<Record<string, string>>,
  queryValues: QueryValues,
  protocolVersion: SupportedProtocolVersion,
  ifNoneMatch?: EntityTag,
  signal?: AbortSignal,
): BuiltReadRequest {
  const path = interpolatePath(operation.pathTemplate, pathValues);
  const query = new URLSearchParams();
  for (const name of operation.queryNames) {
    const value = queryValues[name];
    if (value === undefined) continue;
    if (Array.isArray(value)) {
      for (const item of value) query.append(name, item);
    } else {
      query.append(name, String(value));
    }
  }
  const encodedQuery = query.toString();
  const headers = new Headers();
  if (operation.versioned) {
    headers.set("Teslatlas-Protocol-Version", protocolVersion);
  }

  return {
    path: encodedQuery.length === 0 ? path : `${path}?${encodedQuery}`,
    init: {
      headers,
      redirect: "error",
      ...(ifNoneMatch === undefined ? {} : { ifNoneMatch }),
      ...(signal === undefined ? {} : { signal }),
    },
  };
}

export interface WriteRequestOptions {
  readonly body?: unknown;
  readonly ifMatch?: StrongEntityTag;
  readonly idempotencyKey?: IdempotencyKey;
  readonly signal?: AbortSignal;
  readonly onDispatch?: () => void;
}

export function buildWriteRequest(
  operation: WriteOperationDescriptor,
  pathValues: Readonly<Record<string, string>>,
  protocolVersion: SupportedProtocolVersion,
  options: WriteRequestOptions,
): BuiltReadRequest {
  const path = interpolatePath(operation.pathTemplate, pathValues);
  const headers = new Headers({ "Teslatlas-Protocol-Version": protocolVersion });
  if (options.ifMatch !== undefined) headers.set("If-Match", options.ifMatch);
  if (options.idempotencyKey !== undefined) headers.set("Idempotency-Key", options.idempotencyKey);

  let body: string | undefined;
  if (operation.hasJsonBody) {
    if (options.body === undefined) throw new InvalidRequestBodyError();
    headers.set("Content-Type", "application/json");
    body = stringifyJsonBody(options.body);
  } else if (options.body !== undefined) {
    throw new InvalidRequestBodyError();
  }

  return {
    path,
    init: {
      method: operation.method,
      headers,
      redirect: "error",
      ...(body === undefined ? {} : { body }),
      ...(options.signal === undefined ? {} : { signal: options.signal }),
      ...(options.onDispatch === undefined ? {} : { onDispatch: options.onDispatch }),
    },
  };
}

export class InvalidRequestBodyError extends TeslatlasError<"invalid_request_body"> {
  constructor() {
    super("Protocol request body must be a lossless JSON value", {
      code: "invalid_request_body",
    });
  }
}

function descriptor(
  pathTemplate: string,
  queryNames: readonly string[],
  versioned = true,
): ReadOperationDescriptor {
  return Object.freeze({ pathTemplate, queryNames: Object.freeze(queryNames), versioned });
}

function writeDescriptor(
  pathTemplate: string,
  method: WriteOperationDescriptor["method"],
  hasJsonBody: boolean,
): WriteOperationDescriptor {
  return Object.freeze({ pathTemplate, method, hasJsonBody });
}

function stringifyJsonBody(value: unknown): string {
  try {
    if (!isLosslessJsonValue(value, new Set())) throw new InvalidRequestBodyError();
    const body = JSON.stringify(value);
    if (body === undefined) throw new InvalidRequestBodyError();
    return body;
  } catch (error) {
    if (error instanceof InvalidRequestBodyError) throw error;
    throw new InvalidRequestBodyError();
  }
}

function isLosslessJsonValue(value: unknown, ancestors: Set<object>): boolean {
  if (value === null || typeof value === "string" || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (typeof value !== "object") return false;
  if (ancestors.has(value)) return false;

  ancestors.add(value);
  try {
    if (Array.isArray(value)) return isLosslessJsonArray(value, ancestors);
    return isLosslessJsonObject(value, ancestors);
  } finally {
    ancestors.delete(value);
  }
}

function isLosslessJsonArray(value: unknown[], ancestors: Set<object>): boolean {
  if (Object.getPrototypeOf(value) !== Array.prototype) return false;
  for (const key of Reflect.ownKeys(value)) {
    if (key === "length") continue;
    if (typeof key !== "string" || !isArrayIndex(key, value.length)) return false;
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (descriptor?.enumerable !== true || !("value" in descriptor)) return false;
  }
  for (let index = 0; index < value.length; index += 1) {
    if (!Object.hasOwn(value, index) || !isLosslessJsonValue(value[index], ancestors)) return false;
  }
  return true;
}

function isLosslessJsonObject(value: object, ancestors: Set<object>): boolean {
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) return false;
  for (const key of Reflect.ownKeys(value)) {
    if (typeof key !== "string") return false;
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (descriptor?.enumerable !== true || !("value" in descriptor)) return false;
    if (!isLosslessJsonValue(descriptor.value, ancestors)) return false;
  }
  return true;
}

function isArrayIndex(value: string, length: number): boolean {
  const index = Number(value);
  return Number.isInteger(index) && index >= 0 && index < length && String(index) === value;
}
