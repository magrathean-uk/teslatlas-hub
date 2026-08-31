import type { EntityTag } from "../core/opaque-values.js";
import type { SupportedProtocolVersion } from "../protocol/negotiation.js";
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

function descriptor(
  pathTemplate: string,
  queryNames: readonly string[],
  versioned = true,
): ReadOperationDescriptor {
  return Object.freeze({ pathTemplate, queryNames: Object.freeze(queryNames), versioned });
}
