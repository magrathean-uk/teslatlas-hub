import { describe, expect, it, vi } from "vitest";
import {
  assertNoForbiddenSemanticData,
  assertRedactedSemanticSnapshot,
  canonicalizeSortedJson,
  collectRedactedSemanticSnapshot,
  createReadOnlyCredentialStore,
  hashSortedJson,
  selectSemanticSnapshotPrimaryVehicle,
} from "../../scripts/redacted-semantic-snapshot.mjs";

const vehicleIds = [
  "33333333-3333-4333-8333-333333333333",
  "11111111-1111-4111-8111-111111111111",
  "22222222-2222-4222-8222-222222222222",
];
const drives = [
  drive(105, 1_788_566_200_000, 1_788_566_260_000, 4.2, 1),
  drive(104, 1_788_566_200_000, 1_788_566_260_000, 4.2, 1),
  drive(103, 1_788_566_100_000, 1_788_566_160_000, 4.2, 1),
  drive(102, 1_788_566_000_000, 1_788_566_060_000, 4.2, 1),
  drive(101, 1_788_565_900_000, 1_788_565_960_000, null, null),
];

describe("redacted semantic snapshot", () => {
  it("collects the exact ordered, redacted, half-open semantic contract", async () => {
    const calls: Array<{ vehicleId: string; options: HistoryOptions }> = [];
    const client = {
      vehicles: vi.fn(async () => ({
        value: {
          vehicles: vehicleIds.map((vehicleId, index) => ({
            vehicleId,
            displayName: `private vehicle ${index}`,
          })),
        },
      })),
      current: vi.fn(async (vehicleId: string) => ({
        value:
          vehicleId === vehicleIds[0]
            ? {
                vehicleId,
                observedAtMs: 1_788_566_400_000,
                state: "online",
                batteryLevel: 0,
                estBatteryRangeKm: 160.93,
                odometer: 16_093.44,
                speed: 16,
                insideTemp: 21.5,
                outsideTemp: null,
                chargerPower: null,
                locked: true,
                latitude: 51.5,
                longitude: -0.1,
                displayName: "private current name",
              }
            : unavailableCurrent(vehicleId),
      })),
      drives: vi.fn(async (vehicleId: string, options: HistoryOptions) => {
        calls.push({ vehicleId, options });
        const items =
          vehicleId === vehicleIds[0]
            ? drives
                .filter(
                  (item) => item.startDateMs >= options.fromMs && item.startDateMs < options.toMs,
                )
                .map((item) => ({
                  ...item,
                  vehicleId,
                  startAddress: "private start",
                  endAddress: "private end",
                }))
            : [];
        return { kind: "page", value: { items, nextCursor: null } };
      }),
    };

    const snapshot = await collectRedactedSemanticSnapshot(client);
    const canonical = canonicalizeSortedJson(snapshot);

    expect(snapshot).toMatchObject({
      schema_version: 1,
      profile: "hub-http-v1@1.0.0",
      vehicle_order: ["vehicle-1", "vehicle-2", "vehicle-3"],
      vehicles: [
        {
          label: "vehicle-1",
          current: {
            observed_at_ms: 1_788_566_400_000,
            battery_level: { value: 0, unit: "percent" },
            outside_temperature: { value: null, unit: "degrees Celsius" },
          },
          history: {
            count: 5,
            order: [
              { ordinal: 1, start_date_ms: 1_788_566_200_000 },
              { ordinal: 2, start_date_ms: 1_788_566_200_000 },
              { ordinal: 3, start_date_ms: 1_788_566_100_000 },
              { ordinal: 4, start_date_ms: 1_788_566_000_000 },
              {
                ordinal: 5,
                start_date_ms: 1_788_565_900_000,
                distance: { value: null, unit: "km" },
              },
            ],
          },
        },
        { label: "vehicle-2", current: { observed_at_ms: null, state: "unavailable" } },
        { label: "vehicle-3", current: { observed_at_ms: null, state: "unavailable" } },
      ],
      history_boundary_checks: [
        {
          window: "before-equal-start",
          from_ms: 1_788_565_900_000,
          to_ms: 1_788_566_200_000,
          count: 3,
        },
        {
          window: "equal-start-only",
          from_ms: 1_788_566_200_000,
          to_ms: 1_788_566_200_001,
          count: 2,
        },
      ],
    });
    expect(calls).toHaveLength(5);
    expect(calls.map(({ options }) => [options.fromMs, options.toMs])).toEqual([
      [1_788_565_900_000, 1_788_566_200_001],
      [1_788_565_900_000, 1_788_566_200_001],
      [1_788_565_900_000, 1_788_566_200_001],
      [1_788_565_900_000, 1_788_566_200_000],
      [1_788_566_200_000, 1_788_566_200_001],
    ]);
    expect(canonical).not.toContain("\n");
    expect(new TextEncoder().encode(canonical)).toHaveLength(2959);
    expect(await hashSortedJson(snapshot)).toBe(
      "20b164b499673ba207136dcb0107d8c8d74170029721125442914b99f3ae173a",
    );
    expect(() => assertRedactedSemanticSnapshot(snapshot)).not.toThrow();
  });

  it("sorts object keys recursively while preserving array order", () => {
    expect(canonicalizeSortedJson({ z: [{ b: 2, a: 1 }], a: 0 })).toBe(
      '{"a":0,"z":[{"a":1,"b":2}]}',
    );
  });

  it("keeps the caller credential read-only", () => {
    const credential = { accessToken: "private" };
    const store = createReadOnlyCredentialStore(credential);

    expect(store.load()).toBe(credential);
    expect(() => store.save({ accessToken: "replacement" })).toThrow(
      "attempted to save credentials",
    );
    expect(() => store.clear()).toThrow("attempted to clear credentials");
  });

  it("uses a private four-vehicle profile and derives boundaries from the latest primary drive", async () => {
    const ids = [
      "11111111-1111-4111-8111-111111111111",
      "22222222-2222-4222-8222-222222222222",
      "44444444-4444-4444-8444-444444444444",
      "33333333-3333-4333-8333-333333333333",
    ] as const;
    const profile = {
      schemaVersion: 1 as const,
      primaryVehicleId: ids[2],
      vehicleCount: 4,
      history: { fromMs: 1_000, toMs: 2_001 },
      boundaryMode: "derived-latest-drive" as const,
    };
    const sourceVehicles = ids.map((vehicleId) => ({ vehicleId, displayName: "private" }));
    const primaryDrives = [
      drive(30, 1_800, 1_900, 0, 0),
      drive(29, 1_800, 1_850, null, null),
      drive(28, 1_500, 1_600, 2.5, 2),
    ];
    const calls: Array<{ vehicleId: string; options: HistoryOptions }> = [];
    const client = {
      vehicles: vi.fn(async () => ({ value: { vehicles: sourceVehicles } })),
      current: vi.fn(async (vehicleId: string) => ({
        value:
          vehicleId === ids[2]
            ? {
                ...unavailableCurrent(vehicleId),
                observedAtMs: 2_500,
                state: "online",
                batteryLevel: 0,
              }
            : vehicleId === ids[0]
              ? { ...unavailableCurrent(vehicleId), state: "offline" }
              : unavailableCurrent(vehicleId),
      })),
      drives: vi.fn(async (vehicleId: string, options: HistoryOptions) => {
        calls.push({ vehicleId, options });
        const items =
          vehicleId === ids[2]
            ? primaryDrives.filter(
                (item) => item.startDateMs >= options.fromMs && item.startDateMs < options.toMs,
              )
            : [];
        return { kind: "page", value: { items, nextCursor: null } };
      }),
    };

    expect(selectSemanticSnapshotPrimaryVehicle(sourceVehicles, profile)).toBe(ids[2]);
    const snapshot = (await collectRedactedSemanticSnapshot(client, profile)) as {
      vehicle_order: string[];
      vehicles: Array<{
        label: string;
        current: {
          observed_at_ms: number | null;
          state: string;
          battery_level: { value: number | null };
        };
        history: { query: { from_ms: number; to_ms: number }; count: number };
      }>;
      history_boundary_checks: Array<{
        window: string;
        from_ms: number;
        to_ms: number;
        count: number;
      }>;
    };

    expect(snapshot.vehicle_order).toEqual(["vehicle-1", "vehicle-2", "vehicle-3", "vehicle-4"]);
    expect(snapshot.vehicles).toHaveLength(4);
    expect(snapshot.vehicles[0]).toMatchObject({
      label: "vehicle-1",
      current: { observed_at_ms: 2_500, battery_level: { value: 0 } },
      history: { query: { from_ms: 1_000, to_ms: 2_001 }, count: 3 },
    });
    expect(snapshot.history_boundary_checks).toEqual([
      { window: "before-equal-start", from_ms: 1_000, to_ms: 1_800, count: 1 },
      { window: "equal-start-only", from_ms: 1_800, to_ms: 1_801, count: 2 },
    ]);
    expect(snapshot.vehicles[1]?.current).toMatchObject({
      observed_at_ms: null,
      state: "offline",
    });
    expect(calls).toHaveLength(6);
    expect(calls[0]?.vehicleId).toBe(ids[2]);
    expect(canonicalizeSortedJson(snapshot)).not.toMatch(
      /11111111|22222222|33333333|44444444|private/u,
    );
    expect(() => assertRedactedSemanticSnapshot(snapshot)).not.toThrow();
  });

  it("rejects non-finite numbers and additional fields", async () => {
    expect(() => canonicalizeSortedJson({ value: Number.NaN })).toThrow("non-finite");
    const snapshot = await collectRedactedSemanticSnapshot(mockClient());
    expect(() =>
      assertRedactedSemanticSnapshot({ ...(snapshot as Record<string, unknown>), extra: true }),
    ).toThrow("unexpected shape");
  });

  it.each([
    ["uuid", "private"],
    ["display-name", "private"],
    ["start_address", "private"],
    ["location", "private"],
    ["access_token", "private"],
    ["next_cursor", "private"],
    ["endpoint", "private"],
    ["safe", "11111111-1111-4111-8111-111111111111"],
  ])("recursively rejects forbidden semantic data %s", (key, value) => {
    expect(() => assertNoForbiddenSemanticData({ nested: [{ [key]: value }] })).toThrow(
      /forbidden field|forbidden string/u,
    );
  });
});

function drive(
  id: number,
  startDateMs: number,
  endDateMs: number,
  distanceKm: number | null,
  durationMin: number | null,
) {
  return { id, startDateMs, endDateMs, distanceKm, durationMin };
}

interface HistoryOptions {
  fromMs: number;
  toMs: number;
  limit: number;
  cursor?: string;
}

function unavailableCurrent(vehicleId: string) {
  return {
    vehicleId,
    observedAtMs: null,
    state: null,
    batteryLevel: null,
    estBatteryRangeKm: null,
    odometer: null,
    speed: null,
    insideTemp: null,
    outsideTemp: null,
    chargerPower: null,
    locked: null,
  };
}

function mockClient() {
  return {
    vehicles: async () => ({
      value: { vehicles: vehicleIds.map((vehicleId) => ({ vehicleId, displayName: null })) },
    }),
    current: async (vehicleId: string) => ({ value: unavailableCurrent(vehicleId) }),
    drives: async () => ({ kind: "page", value: { items: [], nextCursor: null } }),
  };
}
