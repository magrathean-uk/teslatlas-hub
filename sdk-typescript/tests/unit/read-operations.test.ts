import chargeSamples from "../../protocol/source/examples/charge-samples-page.json" with {
  type: "json",
};
import charges from "../../protocol/source/examples/charges-page.json" with { type: "json" };
import currentState from "../../protocol/source/examples/current-state.json" with { type: "json" };
import dataQuality from "../../protocol/source/examples/data-quality-page.json" with {
  type: "json",
};
import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import drives from "../../protocol/source/examples/drives-page.json" with { type: "json" };
import positions from "../../protocol/source/examples/positions-page.json" with { type: "json" };
import states from "../../protocol/source/examples/states-page.json" with { type: "json" };
import updates from "../../protocol/source/examples/updates-page.json" with { type: "json" };
import vehicles from "../../protocol/source/examples/vehicles-page.json" with { type: "json" };
import { describe, expect, it } from "vitest";
import { TeslatlasClient } from "../../src/client/client.js";
import type { ClientSession } from "../../src/client/types.js";
import { MissingCapabilityError } from "../../src/core/errors.js";
import { asOpaqueCursor } from "../../src/core/opaque-values.js";
import { FetchTransport, type FetchImplementation } from "../../src/http/fetch-transport.js";
import type { HubDescriptor } from "../../src/protocol/models.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";
import { validateDiscovery } from "../../src/generated/validators.js";

const descriptor = decodeProtocolValue<HubDescriptor>(discovery, validateDiscovery, "discovery");

const reads = [
  ["discoverHub", "/.well-known/teslatlas-hub"],
  ["listVehicles", "/v1/vehicles"],
  ["getVehicleCurrentState", "/v1/vehicles/vehicle%2Fdemo/current"],
  ["listVehicleDrives", "/v1/vehicles/vehicle%2Fdemo/drives"],
  ["getDrive", "/v1/drives/drive%2Fdemo"],
  ["listDrivePositions", "/v1/drives/drive%2Fdemo/positions"],
  ["listVehicleCharges", "/v1/vehicles/vehicle%2Fdemo/charges"],
  ["getCharge", "/v1/charges/charge%2Fdemo"],
  ["listChargeSamples", "/v1/charges/charge%2Fdemo/samples"],
  ["listVehicleStates", "/v1/vehicles/vehicle%2Fdemo/states"],
  ["listVehicleUpdates", "/v1/vehicles/vehicle%2Fdemo/updates"],
  ["listDataQuality", "/v1/data-quality"],
] as const;

describe("typed read operations", () => {
  it("exposes the validated descriptor and negotiated protocol version", () => {
    const client = createClient([]);

    expect(client.descriptor).toBe(descriptor);
    expect(client.protocolVersion).toBe("1.2.0");
  });

  it("gives every released read operation one named method and exact closed path", async () => {
    const observed: ObservedRequest[] = [];
    const client = createClient(observed);

    for (const [method] of reads) {
      await invokeRead(client, method);
    }

    expect(observed.map(({ path }) => path)).toEqual(reads.map(([, path]) => path));
    expect(observed[0]).toMatchObject({ authorization: null, protocolVersion: null });
    for (const request of observed.slice(1)) {
      expect(request).toMatchObject({
        authorization: "Bearer caller-owned",
        protocolVersion: "1.2.0",
      });
    }
  });

  it("emits only declared page/history/data-quality queries", async () => {
    const observed: ObservedRequest[] = [];
    const client = createClient(observed);
    const cursor = asOpaqueCursor("opaque_cursor_0001");

    await client.listVehicleDrives("vehicle_demo", {
      cursor,
      limit: 50,
      from: "2026-01-01T00:00:00.000Z",
      to: "2026-01-02T00:00:00.000Z",
    });
    await client.listDataQuality({
      cursor,
      limit: 25,
      from: "2026-01-01T00:00:00.000Z",
      to: "2026-01-02T00:00:00.000Z",
      vehicleId: "vehicle_demo",
    });

    expect(observed.map(({ path }) => path)).toEqual([
      "/v1/vehicles/vehicle_demo/drives?cursor=opaque_cursor_0001&limit=50&from=2026-01-01T00%3A00%3A00.000Z&to=2026-01-02T00%3A00%3A00.000Z",
      "/v1/data-quality?cursor=opaque_cursor_0001&limit=25&from=2026-01-01T00%3A00%3A00.000Z&to=2026-01-02T00%3A00%3A00.000Z&vehicle_id=vehicle_demo",
    ]);
  });

  it.each([
    ["too-short id", () => createClient([]).getDrive("x")],
    ["oversize id", () => createClient([]).getCharge("x".repeat(129))],
    ["zero limit", () => createClient([]).listVehicles({ limit: 0 })],
    ["fractional limit", () => createClient([]).listVehicles({ limit: 1.5 })],
    ["oversize limit", () => createClient([]).listVehicles({ limit: 501 })],
    [
      "non-millisecond timestamp",
      () => createClient([]).listVehicleDrives("vehicle_demo", { from: "2026-01-01T00:00:00Z" }),
    ],
    [
      "inverted range",
      () =>
        createClient([]).listVehicleDrives("vehicle_demo", {
          from: "2026-01-02T00:00:00.000Z",
          to: "2026-01-01T00:00:00.000Z",
        }),
    ],
  ])("rejects %s before Fetch", async (_label, invoke) => {
    await expect(invoke()).rejects.toMatchObject({ code: "invalid_read_options" });
  });

  it("requires operation capability before Fetch", async () => {
    let calls = 0;
    const withoutHistory: HubDescriptor = {
      ...descriptor,
      capabilities: descriptor.capabilities.filter(({ id }) => id !== "query.history"),
    };
    const client = createClient([], withoutHistory, async () => {
      calls += 1;
      return Response.json(drives);
    });

    await expect(client.listVehicleDrives("vehicle_demo")).rejects.toBeInstanceOf(
      MissingCapabilityError,
    );
    expect(calls).toBe(0);
  });
});

interface ObservedRequest {
  readonly path: string;
  readonly authorization: string | null;
  readonly protocolVersion: string | null;
}

function createClient(
  observed: ObservedRequest[],
  sessionDescriptor: HubDescriptor = descriptor,
  overrideFetch?: FetchImplementation,
): TeslatlasClient {
  const fetch: FetchImplementation =
    overrideFetch ??
    (async (input, init) => {
      const url = new URL(String(input));
      observed.push({
        path: `${url.pathname}${url.search}`,
        authorization: new Headers(init?.headers).get("authorization"),
        protocolVersion: new Headers(init?.headers).get("teslatlas-protocol-version"),
      });
      return Response.json(bodyForPath(url.pathname), {
        headers: { ETag: 'W/"revision-1"' },
      });
    });
  const session: ClientSession = {
    descriptor: sessionDescriptor,
    protocolVersion: "1.2.0",
    discoveryTransport: new FetchTransport({ baseUrl: "https://hub.example.invalid", fetch }),
    apiTransport: new FetchTransport({
      baseUrl: "https://api.example.invalid",
      authorization: () => "Bearer caller-owned",
      fetch,
    }),
    eventTransport: new FetchTransport({ baseUrl: "https://events.example.invalid", fetch }),
  };
  return new TeslatlasClient(session);
}

function bodyForPath(path: string): unknown {
  if (path === "/.well-known/teslatlas-hub") return discovery;
  if (path === "/v1/vehicles") return vehicles;
  if (path.endsWith("/current")) return currentState;
  if (path.endsWith("/drives")) return drives;
  if (path.startsWith("/v1/drives/") && path.endsWith("/positions")) return positions;
  if (path.startsWith("/v1/drives/")) return drives.items[0];
  if (path.endsWith("/charges")) return charges;
  if (path.startsWith("/v1/charges/") && path.endsWith("/samples")) return chargeSamples;
  if (path.startsWith("/v1/charges/")) return charges.items[0];
  if (path.endsWith("/states")) return states;
  if (path.endsWith("/updates")) return updates;
  if (path === "/v1/data-quality") return dataQuality;
  throw new Error(`Unhandled test path: ${path}`);
}

async function invokeRead(
  client: TeslatlasClient,
  method: (typeof reads)[number][0],
): Promise<void> {
  switch (method) {
    case "discoverHub":
      await client.discoverHub();
      return;
    case "listVehicles":
      await client.listVehicles();
      return;
    case "getVehicleCurrentState":
      await client.getVehicleCurrentState("vehicle/demo");
      return;
    case "listVehicleDrives":
      await client.listVehicleDrives("vehicle/demo");
      return;
    case "getDrive":
      await client.getDrive("drive/demo");
      return;
    case "listDrivePositions":
      await client.listDrivePositions("drive/demo");
      return;
    case "listVehicleCharges":
      await client.listVehicleCharges("vehicle/demo");
      return;
    case "getCharge":
      await client.getCharge("charge/demo");
      return;
    case "listChargeSamples":
      await client.listChargeSamples("charge/demo");
      return;
    case "listVehicleStates":
      await client.listVehicleStates("vehicle/demo");
      return;
    case "listVehicleUpdates":
      await client.listVehicleUpdates("vehicle/demo");
      return;
    case "listDataQuality":
      await client.listDataQuality();
  }
}
