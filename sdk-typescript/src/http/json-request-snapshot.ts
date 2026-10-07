import { ProtocolValidationError, TeslatlasError } from "../core/errors.js";

export interface JsonRequestSnapshot {
  readonly value: unknown;
  readonly body: string;
}

const snapshots = new WeakSet<object>();

export class InvalidRequestBodyError extends TeslatlasError<"invalid_request_body"> {
  constructor() {
    super("Protocol request body must be a lossless JSON value", {
      code: "invalid_request_body",
    });
  }
}

/** Copy data descriptors once; neither validation nor serialization sees the caller again. */
export function snapshotJsonRequest(value: unknown, maximumBytes?: number): JsonRequestSnapshot {
  let snapshot: JsonRequestSnapshot;
  try {
    snapshot = Object.freeze(copyJsonValue(value, new Set()));
  } catch {
    throw new InvalidRequestBodyError();
  }
  if (
    maximumBytes !== undefined &&
    new TextEncoder().encode(snapshot.body).byteLength > maximumBytes
  ) {
    throw new ProtocolValidationError("request.body.size");
  }
  snapshots.add(snapshot);
  return snapshot;
}

export function isJsonRequestSnapshot(value: unknown): value is JsonRequestSnapshot {
  return value !== null && typeof value === "object" && snapshots.has(value);
}

function copyJsonValue(value: unknown, ancestors: Set<object>): JsonRequestSnapshot {
  if (value === null || typeof value === "string" || typeof value === "boolean") {
    return { value, body: JSON.stringify(value) };
  }
  if (typeof value === "number" && Number.isFinite(value)) {
    return { value, body: JSON.stringify(value) };
  }
  if (typeof value !== "object" || value === null || ancestors.has(value)) {
    throw new InvalidRequestBodyError();
  }
  ancestors.add(value);
  try {
    if (Array.isArray(value)) return copyJsonArray(value, ancestors);
    return copyJsonObject(value, ancestors);
  } finally {
    ancestors.delete(value);
  }
}

function copyJsonArray(value: unknown[], ancestors: Set<object>): JsonRequestSnapshot {
  if (Object.getPrototypeOf(value) !== Array.prototype) throw new InvalidRequestBodyError();
  const lengthDescriptor = Object.getOwnPropertyDescriptor(value, "length");
  const length: unknown = lengthDescriptor?.value;
  if (typeof length !== "number" || !Number.isInteger(length) || length < 0) {
    throw new InvalidRequestBodyError();
  }
  const keys = Reflect.ownKeys(value);
  if (keys.length !== length + 1) throw new InvalidRequestBodyError();
  const children: JsonRequestSnapshot[] = [];
  for (const key of keys) {
    if (key === "length") continue;
    const index = typeof key === "string" ? Number(key) : Number.NaN;
    if (!Number.isInteger(index) || index < 0 || index >= length || String(index) !== key) {
      throw new InvalidRequestBodyError();
    }
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (descriptor?.enumerable !== true || !("value" in descriptor)) {
      throw new InvalidRequestBodyError();
    }
    children[index] = copyJsonValue(descriptor.value, ancestors);
  }
  return {
    value: Object.freeze(children.map((child) => child.value)),
    body: `[${children.map((child) => child.body).join(",")}]`,
  };
}

function copyJsonObject(value: object, ancestors: Set<object>): JsonRequestSnapshot {
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) throw new InvalidRequestBodyError();
  const copy: Record<string, unknown> = Object.create(null);
  const members: string[] = [];
  for (const key of Reflect.ownKeys(value)) {
    if (typeof key !== "string") throw new InvalidRequestBodyError();
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (descriptor?.enumerable !== true || !("value" in descriptor)) {
      throw new InvalidRequestBodyError();
    }
    const child = copyJsonValue(descriptor.value, ancestors);
    copy[key] = child.value;
    members.push(`${JSON.stringify(key)}:${child.body}`);
  }
  return { value: Object.freeze(copy), body: `{${members.join(",")}}` };
}
