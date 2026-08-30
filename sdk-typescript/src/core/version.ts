import { TeslatlasError } from "./errors.js";

const semanticVersionPattern =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/u;

export interface ProtocolVersion {
  readonly major: number;
  readonly minor: number;
  readonly patch: number;
  readonly prerelease?: string;
  readonly build?: string;
}

export interface ProtocolCompatibilityWindow {
  readonly major: number;
  readonly minimumMinor: number;
  readonly maximumMinor: number;
}

export type ProtocolCompatibility =
  | { readonly compatible: true }
  | {
      readonly compatible: false;
      readonly reason: "unsupported-major" | "below-minimum-minor" | "above-maximum-minor";
    };

export class InvalidProtocolVersionError extends TeslatlasError<"invalid_protocol_version"> {
  constructor() {
    super("Protocol version must be a valid semantic version", {
      code: "invalid_protocol_version",
    });
  }
}

export class InvalidCompatibilityWindowError extends TeslatlasError<"invalid_compatibility_window"> {
  constructor() {
    super("Protocol compatibility window is invalid", {
      code: "invalid_compatibility_window",
    });
  }
}

export function parseProtocolVersion(value: string): ProtocolVersion {
  const match = semanticVersionPattern.exec(value);
  if (match === null) {
    throw new InvalidProtocolVersionError();
  }

  const major = parseSafeInteger(match[1]);
  const minor = parseSafeInteger(match[2]);
  const patch = parseSafeInteger(match[3]);
  const prerelease = match[4];
  const build = match[5];

  if (
    major === undefined ||
    minor === undefined ||
    patch === undefined ||
    hasInvalidNumericPrerelease(prerelease)
  ) {
    throw new InvalidProtocolVersionError();
  }

  return Object.freeze({
    major,
    minor,
    patch,
    ...(prerelease === undefined ? {} : { prerelease }),
    ...(build === undefined ? {} : { build }),
  });
}

export function checkProtocolVersion(
  version: ProtocolVersion,
  window: ProtocolCompatibilityWindow,
): ProtocolCompatibility {
  assertCompatibilityWindow(window);

  if (version.major !== window.major) {
    return { compatible: false, reason: "unsupported-major" };
  }
  if (version.minor < window.minimumMinor) {
    return { compatible: false, reason: "below-minimum-minor" };
  }
  if (version.minor > window.maximumMinor) {
    return { compatible: false, reason: "above-maximum-minor" };
  }
  return { compatible: true };
}

function assertCompatibilityWindow(window: ProtocolCompatibilityWindow): void {
  if (
    !isNonNegativeSafeInteger(window.major) ||
    !isNonNegativeSafeInteger(window.minimumMinor) ||
    !isNonNegativeSafeInteger(window.maximumMinor) ||
    window.minimumMinor > window.maximumMinor
  ) {
    throw new InvalidCompatibilityWindowError();
  }
}

function parseSafeInteger(value: string | undefined): number | undefined {
  if (value === undefined) {
    return undefined;
  }
  const parsed = Number(value);
  return isNonNegativeSafeInteger(parsed) ? parsed : undefined;
}

function isNonNegativeSafeInteger(value: number): boolean {
  return Number.isSafeInteger(value) && value >= 0;
}

function hasInvalidNumericPrerelease(value: string | undefined): boolean {
  if (value === undefined) {
    return false;
  }
  return value
    .split(".")
    .some((identifier) => /^\d+$/u.test(identifier) && /^0\d+/u.test(identifier));
}
