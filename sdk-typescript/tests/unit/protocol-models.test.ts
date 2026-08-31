import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import vehicles from "../../protocol/source/examples/vehicles-page.json" with { type: "json" };
import { describe, expect, it } from "vitest";
import { ProtocolValidationError } from "../../src/core/errors.js";
import { validateDiscovery, validateVehiclePage } from "../../src/generated/validators.js";
import type { HubDescriptor, VehiclePage } from "../../src/protocol/models.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";

describe("protocol models", () => {
  it("uses generated declarations for canonical protocol examples", () => {
    const descriptor: HubDescriptor = decodeProtocolValue(
      discovery,
      validateDiscovery,
      "validateDiscovery",
    );
    const page: VehiclePage = decodeProtocolValue(
      vehicles,
      validateVehiclePage,
      "validateVehiclePage",
    );

    expect(descriptor.protocol.current_version).toBe("1.2.0");
    expect(page.items.length).toBeGreaterThan(0);
  });

  it("returns a value only after its generated validator accepts it", () => {
    expect(decodeProtocolValue(discovery, validateDiscovery, "validateDiscovery")).toBe(discovery);
  });

  it("reports only the validator name for invalid protocol data", () => {
    const value = { secret: "Bearer must-not-leak" };

    const error = captureError(() =>
      decodeProtocolValue(value, validateDiscovery, "validateDiscovery"),
    );

    expect(error).toMatchObject({
      name: "ProtocolValidationError",
      code: "protocol_validation",
      validator: "validateDiscovery",
      message: "Teslatlas protocol response is invalid",
    });
    expect(error).toBeInstanceOf(ProtocolValidationError);
    expect(error).not.toHaveProperty("cause");
    expect(error).not.toHaveProperty("errors");
    expect(String(error)).not.toContain("must-not-leak");
  });
});

function captureError(operation: () => unknown): unknown {
  try {
    operation();
    return undefined;
  } catch (error) {
    return error;
  }
}
