import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import { describe, expect, it } from "vitest";
import { IncompatibleProtocolError, MissingCapabilityError } from "../../src/core/errors.js";
import { validateDiscovery } from "../../src/generated/validators.js";
import { requireCapability } from "../../src/protocol/capabilities.js";
import type { HubDescriptor } from "../../src/protocol/models.js";
import { negotiateProtocolVersion } from "../../src/protocol/negotiation.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";

const canonicalDescriptor = decodeProtocolValue<HubDescriptor>(
  discovery,
  validateDiscovery,
  "validateDiscovery",
);

describe("protocol negotiation", () => {
  it("selects the highest locked Hub version not newer than the request", () => {
    expect(negotiateProtocolVersion(canonicalDescriptor, "1.2.0")).toBe("1.2.0");
    expect(negotiateProtocolVersion(canonicalDescriptor, "1.1.0")).toBe("1.1.0");

    const descriptor = withSupportedVersions(["1.0.0", "1.1.0", "1.3.0"]);
    expect(negotiateProtocolVersion(descriptor, "1.2.0")).toBe("1.1.0");
  });

  it("rejects incompatible client majors and minimum-client constraints", () => {
    expect(() => negotiateProtocolVersion(canonicalDescriptor, "2.0.0")).toThrow(
      IncompatibleProtocolError,
    );

    const descriptor = {
      ...canonicalDescriptor,
      protocol: { ...canonicalDescriptor.protocol, minimum_client_version: "1.3.0" },
    };
    expect(() => negotiateProtocolVersion(descriptor, "1.2.0")).toThrow(IncompatibleProtocolError);
  });

  it("accepts a requested version above a lower-major minimum", () => {
    const descriptor = {
      ...canonicalDescriptor,
      protocol: { ...canonicalDescriptor.protocol, minimum_client_version: "0.9.0" },
    };

    expect(negotiateProtocolVersion(descriptor, "1.2.0")).toBe("1.2.0");
  });

  it("returns an advertised capability and rejects an absent one", () => {
    expect(requireCapability(canonicalDescriptor, "commands.async").id).toBe("commands.async");

    const error = captureError(() => requireCapability(canonicalDescriptor, "missing.capability"));
    expect(error).toBeInstanceOf(MissingCapabilityError);
    expect(error).toMatchObject({ capability: "missing.capability" });
  });
});

function withSupportedVersions(supportedVersions: string[]): HubDescriptor {
  return {
    ...canonicalDescriptor,
    protocol: { ...canonicalDescriptor.protocol, supported_versions: supportedVersions },
  };
}

function captureError(operation: () => unknown): unknown {
  try {
    operation();
    return undefined;
  } catch (error) {
    return error;
  }
}
