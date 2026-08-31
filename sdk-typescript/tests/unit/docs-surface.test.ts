import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";

const documentation = [
  "../../README.md",
  "../../docs/architecture.md",
  "../../docs/api.md",
  "../../docs/protocol-dependency-gate.md",
  "../../docs/plans/2026-08-30-foundation.md",
] as const;

describe("published documentation", () => {
  it("describes the implemented client instead of the obsolete transport foundation", async () => {
    const pages = await Promise.all(
      documentation.map((path) => readFile(new URL(path, import.meta.url), "utf8")),
    );
    const [readme, architecture, api, gate, foundation] = pages;

    expect(readme).toContain("createClient");
    expect(architecture).toContain("TeslatlasClient");
    expect(api).toContain("listVehicles");
    expect(gate).toContain("79ced4c7fdc79520ad31d72a0280bf5f3f19f407");
    expect(foundation).toContain("executed");
    expect(pages.join("\n")).not.toContain("Transport foundation under development");
    expect(pages.join("\n")).not.toContain("Resource-level SDK development is blocked");
  });

  it("ships a compatibility guide with the locked protocol and every public operation", async () => {
    const compatibility = await readFile(
      new URL("../../docs/compatibility.md", import.meta.url),
      "utf8",
    );

    expect(compatibility).toContain("79ced4c7fdc79520ad31d72a0280bf5f3f19f407");
    expect(compatibility).toContain("1.0.0");
    expect(compatibility).toContain("1.1.0");
    expect(compatibility).toContain("1.2.0");
    expect(compatibility).toContain("npm run protocol:generate");
    expect(compatibility).toContain("SDK protocol-case evidence is not server conformance");
    for (const method of [
      "discoverHub",
      "listVehicles",
      "getVehicleCurrentState",
      "listVehicleDrives",
      "getDrive",
      "listDrivePositions",
      "listVehicleCharges",
      "getCharge",
      "listChargeSamples",
      "listVehicleStates",
      "listVehicleUpdates",
      "streamEvents",
      "listDataQuality",
      "createCommand",
      "getCommand",
      "listVehicleMetadata",
      "createMetadata",
      "getMetadata",
      "replaceMetadata",
      "deleteMetadata",
    ]) {
      expect(compatibility).toContain(`\`${method}\``);
    }
  });
});
