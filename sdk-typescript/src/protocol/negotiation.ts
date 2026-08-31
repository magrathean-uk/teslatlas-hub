import { IncompatibleProtocolError } from "../core/errors.js";
import { parseProtocolVersion, type ProtocolVersion } from "../core/version.js";
import type { HubDescriptor } from "./models.js";

export type SupportedProtocolVersion = "1.0.0" | "1.1.0" | "1.2.0";

const lockedProtocolVersions = ["1.0.0", "1.1.0", "1.2.0"] as const;

export function negotiateProtocolVersion(
  descriptor: HubDescriptor,
  requestedVersion: string,
): SupportedProtocolVersion {
  const requested = parseProtocolVersion(requestedVersion);
  const current = parseProtocolVersion(descriptor.protocol.current_version);
  const minimumClient = parseProtocolVersion(descriptor.protocol.minimum_client_version);
  const lockedMajor = parseProtocolVersion(lockedProtocolVersions[0]).major;

  if (
    requested.major !== lockedMajor ||
    current.major !== lockedMajor ||
    compareVersions(requested, minimumClient) < 0
  ) {
    throw new IncompatibleProtocolError();
  }

  const advertised = new Set(descriptor.protocol.supported_versions);
  for (const version of lockedProtocolVersions.toReversed()) {
    const candidate = parseProtocolVersion(version);
    if (
      advertised.has(version) &&
      compareVersions(candidate, requested) <= 0 &&
      compareVersions(candidate, minimumClient) >= 0
    ) {
      return version;
    }
  }

  throw new IncompatibleProtocolError();
}

function compareVersions(left: ProtocolVersion, right: ProtocolVersion): number {
  return left.major - right.major || left.minor - right.minor || left.patch - right.patch;
}
