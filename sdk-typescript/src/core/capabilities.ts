import { containsControlCharacters, TeslatlasError } from "./errors.js";

const capabilityIdBrand: unique symbol = Symbol("CapabilityId");

export type CapabilityId = string & {
  readonly [capabilityIdBrand]: true;
};

export type CapabilitySet = ReadonlySet<CapabilityId>;

export class InvalidCapabilityError extends TeslatlasError<"invalid_capability"> {
  constructor() {
    super("Capability identifier must be non-empty and contain no control characters", {
      code: "invalid_capability",
    });
  }
}

export class MissingCapabilitiesError extends TeslatlasError<"missing_capabilities"> {
  readonly missing: readonly CapabilityId[];

  constructor(missing: readonly CapabilityId[]) {
    super("Required Teslatlas capabilities are unavailable", {
      code: "missing_capabilities",
    });
    this.missing = Object.freeze([...missing]);
  }
}

export function asCapabilityId(value: string): CapabilityId {
  if (value.length === 0 || containsControlCharacters(value)) {
    throw new InvalidCapabilityError();
  }
  return value as CapabilityId;
}

export function createCapabilitySet(values: Iterable<string>): CapabilitySet {
  const capabilities = new Set<CapabilityId>();
  for (const value of values) {
    capabilities.add(asCapabilityId(value));
  }
  return capabilities;
}

export function findMissingCapabilities(
  available: CapabilitySet,
  required: Iterable<string>,
): readonly CapabilityId[] {
  const missing = new Set<CapabilityId>();
  for (const value of required) {
    const capability = asCapabilityId(value);
    if (!available.has(capability)) {
      missing.add(capability);
    }
  }
  return Object.freeze([...missing].sort(compareOpaqueStrings));
}

export function requireCapabilities(available: CapabilitySet, required: Iterable<string>): void {
  const missing = findMissingCapabilities(available, required);
  if (missing.length > 0) {
    throw new MissingCapabilitiesError(missing);
  }
}

function compareOpaqueStrings(left: string, right: string): number {
  if (left < right) {
    return -1;
  }
  if (left > right) {
    return 1;
  }
  return 0;
}
