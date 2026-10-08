import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";
import {
  decodeHubJson,
  validateHubCurrent,
  validateHubDiscovery,
  validateHubDrives,
} from "../../src/hub/validate.js";

const profileRoot = new URL("../../protocol/source/profiles/hub-http-v1/1.0.0/", import.meta.url);

describe("current Hub model decoding", () => {
  it("accepts the frozen discovery bytes with an optional ETag-independent shape", async () => {
    const bytes = await readFile(new URL("examples/discovery.json", profileRoot), "utf8");

    expect(decodeHubJson(bytes, validateHubDiscovery, "HubDiscovery")).toEqual({
      apiVersions: ["1.0"],
      capabilities: ["query.vehicles", "query.current", "query.drives", "sync.packs"],
      hubId: "11111111-1111-4111-8111-111111111111",
      packFormat: "sqlite-zstd",
      protocol: "teslatlas-sync",
      protocolMajor: 1,
      sourceUrl: "https://example.invalid/source",
      version: "2026.36.2",
    });
  });

  it("preserves every nullable current value and the UUID identity", async () => {
    const bytes = await readFile(new URL("examples/current.json", profileRoot), "utf8");

    const current = decodeHubJson(bytes, validateHubCurrent, "HubCurrent");

    expect(current.vehicleId).toBe("11111111-1111-4111-8111-111111111111");
    expect(current.observedAtMs).toBeNull();
    expect(current.scheduledChargingStartTime).toBeNull();
    expect(current.activeRouteMilesToArrival).toBeNull();
    expect(current.car).toBeNull();
    expect(Object.keys(current)).toHaveLength(85);
  });

  it("keeps numeric drive IDs exact when they are safe JavaScript integers", async () => {
    const bytes = await readFile(new URL("examples/drives.json", profileRoot), "utf8");

    const page = decodeHubJson(bytes, validateHubDrives, "HubDrives");

    expect(page.items[0]).toMatchObject({
      id: 101,
      vehicleId: "11111111-1111-4111-8111-111111111111",
      startDateMs: 1_788_565_900_000,
      endDateMs: 1_788_565_960_000,
      efficiency: null,
    });
    expect(page.nextCursor).toBeNull();
  });

  it("rejects integers that JSON.parse would silently round", async () => {
    const bytes = await readFile(new URL("examples/drives.json", profileRoot), "utf8");
    const unsafe = bytes.replace('"id": 101', '"id": 9007199254740993');

    expect(() => decodeHubJson(unsafe, validateHubDrives, "HubDrives")).toThrowError(
      expect.objectContaining({ code: "protocol_validation", validator: "HubDrives.integer" }),
    );
  });

  it.each([
    "101.000000000000001",
    "9007199254740991.1",
    "-101.000000000000001",
    "1.00000000000000001e2",
    "1e-400",
  ])("rejects the exact fractional drive ID %s before camelization", async (token) => {
    const bytes = await readFile(new URL("examples/drives.json", profileRoot), "utf8");
    const fractional = bytes.replace('"id": 101', `"id": ${token}`);

    expect(() => decodeHubJson(fractional, validateHubDrives, "HubDrives")).toThrowError(
      expect.objectContaining({ code: "protocol_validation", validator: "HubDrives.integer" }),
    );
  });

  it.each(["101.0", "1.01e2", "10100e-2", "-101.0", "9007199254740991.0", "-9007199254740991.0"])(
    "retains the exact integral drive ID spelling %s",
    async (token) => {
      const bytes = await readFile(new URL("examples/drives.json", profileRoot), "utf8");
      const integral = bytes.replace('"id": 101', `"id": ${token}`);

      expect(decodeHubJson(integral, validateHubDrives, "HubDrives").items[0]?.id).toBe(
        Number(token),
      );
    },
  );

  it.each(["9007199254740993.0", "-9007199254740993", "9.007199254740993e15"])(
    "rejects an integral drive ID that cannot be represented exactly %s",
    async (token) => {
      const bytes = await readFile(new URL("examples/drives.json", profileRoot), "utf8");
      const unsafe = bytes.replace('"id": 101', `"id": ${token}`);

      expect(() => decodeHubJson(unsafe, validateHubDrives, "HubDrives")).toThrowError(
        expect.objectContaining({ code: "protocol_validation", validator: "HubDrives.integer" }),
      );
    },
  );

  it("keeps ordinary fractional measurements beside exactly integral fields", async () => {
    const bytes = await readFile(new URL("examples/current.json", profileRoot), "utf8");
    const measured = bytes
      .replace('"latitude": null', '"latitude": 51.507351')
      .replace('"inside_temp": null', '"inside_temp": 20.25')
      .replace('"battery_level": null', '"battery_level": 80.0');

    expect(decodeHubJson(measured, validateHubCurrent, "HubCurrent")).toMatchObject({
      latitude: 51.507351,
      insideTemp: 20.25,
      batteryLevel: 80,
    });
    expect(() =>
      decodeHubJson(
        measured.replace('"battery_level": 80.0', '"battery_level": 80.000000000000001'),
        validateHubCurrent,
        "HubCurrent",
      ),
    ).toThrowError(expect.objectContaining({ validator: "HubCurrent.integer" }));
  });

  it("checks an integral discovery constant before rounding", async () => {
    const bytes = await readFile(new URL("examples/discovery.json", profileRoot), "utf8");

    expect(() =>
      decodeHubJson(
        bytes.replace('"protocol_major": 1', '"protocol_major": 1.00000000000000001'),
        validateHubDiscovery,
        "HubDiscovery",
      ),
    ).toThrowError(expect.objectContaining({ validator: "HubDiscovery.integer" }));
  });

  it("preserves final-member JSON parsing for duplicate and escaped equivalent keys", async () => {
    const bytes = await readFile(new URL("examples/drives.json", profileRoot), "utf8");
    const overwritten = bytes.replace('"id": 101', '"id": 101.000000000000001, "i\\u0064": 101');
    const overwrittenOverflow = bytes.replace('"id": 101', '"id": 1e309, "id": 101');
    const effective = bytes.replace('"id": 101', '"id": 101, "i\\u0064": 101.000000000000001');

    expect(decodeHubJson(overwritten, validateHubDrives, "HubDrives").items[0]?.id).toBe(101);
    expect(decodeHubJson(overwrittenOverflow, validateHubDrives, "HubDrives").items[0]?.id).toBe(
      101,
    );
    expect(() => decodeHubJson(effective, validateHubDrives, "HubDrives")).toThrowError(
      expect.objectContaining({ validator: "HubDrives.integer" }),
    );
  });

  it("rejects malformed UUIDs and unknown capability names from exact JSON bytes", async () => {
    const bytes = await readFile(new URL("examples/discovery.json", profileRoot), "utf8");
    const wrongUuid = bytes.replace("11111111-1111-4111-8111-111111111111", "not-a-uuid");
    const wrongCapability = bytes.replace("query.drives", "query.charges");

    expect(() => decodeHubJson(wrongUuid, validateHubDiscovery, "HubDiscovery")).toThrowError(
      expect.objectContaining({ code: "protocol_validation" }),
    );
    expect(() => decodeHubJson(wrongCapability, validateHubDiscovery, "HubDiscovery")).toThrowError(
      expect.objectContaining({ code: "protocol_validation" }),
    );
  });
});
