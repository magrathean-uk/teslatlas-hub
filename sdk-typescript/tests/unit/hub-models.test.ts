import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";
import {
  decodeHubJson,
  validateHubClaim,
  validateHubCurrent,
  validateHubDiscovery,
  validateHubDrives,
  validateHubError,
} from "../../src/hub/validate.js";

const profileRoot = new URL("../../../protocol/profiles/hub-http-v1/1.2.0/", import.meta.url);

type Mutation = (value: Record<string, unknown>) => void;

async function discoveryWith(change: Mutation): Promise<string> {
  const value = JSON.parse(
    await readFile(new URL("examples/discovery.json", profileRoot), "utf8"),
  ) as Record<string, unknown>;
  change(value);
  return JSON.stringify(value);
}

const invalidDiscoveries: ReadonlyArray<readonly [string, Mutation]> = [
  [
    "no query.vehicles",
    (value) => {
      value.capabilities = ["query.current"];
    },
  ],
  [
    "no query.current",
    (value) => {
      value.capabilities = ["query.vehicles"];
    },
  ],
  [
    "a duplicate token",
    (value) => {
      (value.capabilities as string[]).push("query.vehicles");
    },
  ],
  [
    "an overlong token",
    (value) => {
      (value.capabilities as string[]).push("a".repeat(65));
    },
  ],
  [
    "a token outside the grammar",
    (value) => {
      (value.capabilities as string[]).push("Query-Drives");
    },
  ],
  [
    "more than 16 tokens",
    (value) => {
      value.capabilities = [
        "query.vehicles",
        "query.current",
        ...Array.from({ length: 15 }, (_, index) => `future.${index}`),
      ];
    },
  ],
  [
    "no profile",
    (value) => {
      Reflect.deleteProperty(value, "profile");
    },
  ],
  [
    "a profile outside hub-http-v1@1",
    (value) => {
      value.profile = "hub-http-v1@2.0.0";
    },
  ],
];

describe("current Hub model decoding", () => {
  it("accepts the 1.2.0 discovery bytes with an optional ETag-independent shape", async () => {
    const bytes = await readFile(new URL("examples/discovery.json", profileRoot), "utf8");

    expect(decodeHubJson(bytes, validateHubDiscovery, "HubDiscovery")).toEqual({
      apiVersions: ["1.0"],
      attributions: [
        {
          id: "osm",
          licence: "ODbL-1.0",
          text: "(c) OpenStreetMap contributors",
          url: "https://www.openstreetmap.org/copyright",
        },
      ],
      capabilities: [
        "query.vehicles",
        "query.current",
        "query.drives",
        "query.charges",
        "query.charge.samples",
        "query.positions",
        "query.enrichment",
        "sync.packs",
        "auth.revoke",
      ],
      hubId: "11111111-1111-4111-8111-111111111111",
      manifestPublicKey: "0".repeat(64),
      packFormat: "sqlite-zstd",
      profile: "hub-http-v1@1.2.0",
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
    expect(current.car).toMatchObject({
      id: 1,
      name: "Synthetic car",
      settings: { enabled: true, suspendAfterIdleMin: 15, suspendMin: 21 },
    });
    expect(Object.keys(current)).toHaveLength(85);
  });

  it("keeps numeric drive IDs exact when they are safe JavaScript integers", async () => {
    const bytes = await readFile(new URL("examples/drives.json", profileRoot), "utf8");

    const page = decodeHubJson(bytes, validateHubDrives, "HubDrives");

    expect(page.items.map((drive) => drive.id)).toEqual([102, 101]);
    expect(page.items[1]).toMatchObject({
      id: 101,
      vehicleId: "11111111-1111-4111-8111-111111111111",
      startDateMs: 1_788_565_900_000,
      endDateMs: 1_788_565_960_000,
      efficiency: null,
    });
    expect(page.nextCursor).toMatch(/^tqp1\./u);
  });

  it("decodes the address provenance of a drive and keeps its lookup time exact", async () => {
    const bytes = await readFile(new URL("examples/drives.json", profileRoot), "utf8");

    expect(decodeHubJson(bytes, validateHubDrives, "HubDrives").items[0]).toMatchObject({
      endAddressProvenance: {
        endpointHost: "geocoder.example.invalid",
        language: "en",
        licence: "ODbL-1.0",
        lookedUpAtMs: 1_788_565_840_000,
        provider: "synthetic-geocoder",
        source: "geocoder",
      },
      startAddressProvenance: { lookedUpAtMs: null, provider: null, source: "import" },
    });
    expect(
      decodeHubJson(
        bytes.replace('"looked_up_at_ms": 1788565840000', '"looked_up_at_ms": 1788565840000.0'),
        validateHubDrives,
        "HubDrives",
      ).items[0],
    ).toMatchObject({ endAddressProvenance: { lookedUpAtMs: 1_788_565_840_000 } });
    expect(() =>
      decodeHubJson(
        bytes.replace('"looked_up_at_ms": 1788565840000', '"looked_up_at_ms": 1788565840000.5'),
        validateHubDrives,
        "HubDrives",
      ),
    ).toThrowError(expect.objectContaining({ validator: "HubDrives.integer" }));
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

      expect(decodeHubJson(integral, validateHubDrives, "HubDrives").items[1]?.id).toBe(
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

    expect(decodeHubJson(overwritten, validateHubDrives, "HubDrives").items[1]?.id).toBe(101);
    expect(decodeHubJson(overwrittenOverflow, validateHubDrives, "HubDrives").items[1]?.id).toBe(
      101,
    );
    expect(() => decodeHubJson(effective, validateHubDrives, "HubDrives")).toThrowError(
      expect.objectContaining({ validator: "HubDrives.integer" }),
    );
  });

  it("rejects a malformed Hub UUID from exact JSON bytes", async () => {
    const bytes = await readFile(new URL("examples/discovery.json", profileRoot), "utf8");
    const wrongUuid = bytes.replace("11111111-1111-4111-8111-111111111111", "not-a-uuid");

    expect(() => decodeHubJson(wrongUuid, validateHubDiscovery, "HubDiscovery")).toThrowError(
      expect.objectContaining({ code: "protocol_validation" }),
    );
  });

  it("accepts any well-formed capability token and unknown additive fields", async () => {
    const future = await discoveryWith((value) => {
      (value.capabilities as string[]).push("future.feature");
      value.future_field = { nested: true };
    });

    expect(decodeHubJson(future, validateHubDiscovery, "HubDiscovery")).toMatchObject({
      capabilities: expect.arrayContaining(["query.vehicles", "query.current", "future.feature"]),
      futureField: { nested: true },
    });
  });

  it.each(invalidDiscoveries)("rejects discovery with %s", async (_label, change) => {
    const bytes = await discoveryWith(change);

    expect(() => decodeHubJson(bytes, validateHubDiscovery, "HubDiscovery")).toThrowError(
      expect.objectContaining({ code: "protocol_validation" }),
    );
  });

  it("accepts any lowercase error code token and rejects one outside the grammar", async () => {
    const drive = await readFile(
      new URL("examples/error-drive-not-found.json", profileRoot),
      "utf8",
    );

    expect(decodeHubJson(drive, validateHubError, "HubError")).toEqual({
      error: { code: "drive_not_found", message: "drive was not found" },
    });
    expect(
      decodeHubJson(drive.replace("drive_not_found", "future_code"), validateHubError, "HubError"),
    ).toMatchObject({ error: { code: "future_code" } });
    for (const code of ["Future_Code", "future-code", "a".repeat(65), ""]) {
      expect(() =>
        decodeHubJson(drive.replace("drive_not_found", code), validateHubError, "HubError"),
      ).toThrowError(expect.objectContaining({ code: "protocol_validation" }));
    }
  });

  it("accepts the maximum-lifetime rotation reason and nothing else", async () => {
    const bytes = await readFile(new URL("examples/rotate-max-lifetime.json", profileRoot), "utf8");
    const plain = await readFile(new URL("examples/claim.json", profileRoot), "utf8");

    expect(decodeHubJson(bytes, validateHubClaim, "HubClaim")).toMatchObject({
      reason: "max_lifetime",
    });
    expect(decodeHubJson(plain, validateHubClaim, "HubClaim")).not.toHaveProperty("reason");
    expect(() =>
      decodeHubJson(bytes.replace("max_lifetime", "other"), validateHubClaim, "HubClaim"),
    ).toThrowError(expect.objectContaining({ code: "protocol_validation" }));
  });

  describe("exact drive measurements and identifiers", () => {
    // The second example item (id 101) has null measurements, so edits aimed at the first
    // null of a key land on it.
    async function drivesBytes() {
      return readFile(new URL("examples/drives.json", profileRoot), "utf8");
    }

    it("rejects a nonfinite measurement", async () => {
      const raw = (await drivesBytes()).replace('"distance_km": null', '"distance_km": 1e400');

      expect(() => decodeHubJson(raw, validateHubDrives, "HubDrives")).toThrow();
    });

    it("keeps a finite measurement decimal that binary64 rounds", async () => {
      const raw = (await drivesBytes()).replace(
        '"distance_km": null',
        '"distance_km": 42.0000000000000001',
      );

      expect(decodeHubJson(raw, validateHubDrives, "HubDrives").items[1]?.distanceKm).toBe(42);
    });

    it.each(["9007199254740992", "9007199254740994", "101.0", "10100e-2"])(
      "preserves the exact Hub drive ID %s",
      async (token) => {
        const raw = (await drivesBytes()).replace('"id": 101', `"id": ${token}`);

        expect(decodeHubJson(raw, validateHubDrives, "HubDrives").items[1]?.id).toBe(Number(token));
      },
    );

    it.each(["9223372036854774784", "-9223372036854775808"])(
      "preserves the signed-i64 representable ID %s",
      async (token) => {
        const raw = (await drivesBytes()).replace('"id": 101', `"id": ${token}`);

        expect(decodeHubJson(raw, validateHubDrives, "HubDrives").items[1]?.id).toBe(Number(token));
      },
    );

    it("rejects signed-i64 overflow hidden by a rounded schema maximum", async () => {
      const raw = (await drivesBytes()).replace('"id": 101', '"id": 9223372036854775808');

      expect(() => decodeHubJson(raw, validateHubDrives, "HubDrives")).toThrow();
    });
  });
});
