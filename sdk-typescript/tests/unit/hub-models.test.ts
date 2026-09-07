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
    const unsafe = bytes.replace('"id": 101', '"id": 9007199254740992');

    expect(() => decodeHubJson(unsafe, validateHubDrives, "HubDrives")).toThrowError(
      expect.objectContaining({ code: "protocol_validation", validator: "HubDrives.integer" }),
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
