import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import invalidError from "../../protocol/source/examples/invalid/error-success-status.json" with { type: "json" };
import vehicles from "../../protocol/source/examples/vehicles-page.json" with { type: "json" };
import { describe, expect, it } from "vitest";
import { validateDiscovery, validateProblem, validateVehiclePage } from "../../src/generated/validators.js";

describe("generated protocol validators", () => {
  it("accepts canonical discovery and vehicle-page examples", () => {
    expect(validateDiscovery(discovery)).toBe(true);
    expect(validateVehiclePage(vehicles)).toBe(true);
  });

  it("rejects a problem document with a success status", () => {
    expect(validateProblem(invalidError)).toBe(false);
  });
});
