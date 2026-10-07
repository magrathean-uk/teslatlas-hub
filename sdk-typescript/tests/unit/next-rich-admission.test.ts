import { describe, expect, it, vi } from "vitest";
import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import vehicles from "../../protocol/source/examples/vehicles-page.json" with { type: "json" };
import current from "../../protocol/source/examples/current-state.json" with { type: "json" };
import drives from "../../protocol/source/examples/drives-page.json" with { type: "json" };
import positions from "../../protocol/source/examples/positions-page.json" with { type: "json" };
import charges from "../../protocol/source/examples/charges-page.json" with { type: "json" };
import samples from "../../protocol/source/examples/charge-samples-page.json" with { type: "json" };
import states from "../../protocol/source/examples/states-page.json" with { type: "json" };
import updates from "../../protocol/source/examples/updates-page.json" with { type: "json" };
import quality from "../../protocol/source/examples/data-quality-page.json" with { type: "json" };
import metadataPage from "../../protocol/source/examples/metadata-page.json" with { type: "json" };
import metadata from "../../protocol/source/examples/metadata-record.json" with { type: "json" };
import tombstone from "../../protocol/source/examples/metadata-tombstone.json" with {
  type: "json",
};
import command from "../../protocol/source/examples/command-request.json" with { type: "json" };
import job from "../../protocol/source/examples/command-job.json" with { type: "json" };
import { TeslatlasClient } from "../../src/client/client.js";
import { createClientSession } from "../../src/client/session.js";
import { asIdempotencyKey } from "../../src/commands/idempotency.js";
import { asEntityTag } from "../../src/core/opaque-values.js";
import { asStrongEntityTag } from "../../src/http/strong-etag.js";
import type { FetchImplementation } from "../../src/http/fetch-transport.js";
import type { AuthorizationProvider } from "../../src/auth/credential-store.js";
import type { CommandRequest, MetadataCreate } from "../../src/protocol/models.js";
import type { SupportedProtocolVersion } from "../../src/protocol/negotiation.js";

const idempotencyKey = asIdempotencyKey("11111111-1111-4111-8111-111111111111");
const ifMatch = asStrongEntityTag('"record"');
const metadataCreate: MetadataCreate = {
  vehicle_id: metadata.vehicle_id,
  kind: "note",
  target: metadata.target as MetadataCreate["target"],
  value: metadata.value,
};

function pending<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

function first<T>(items: readonly T[]): T {
  const item = items[0];
  if (item === undefined) throw new Error("Expected a nonempty fixture");
  return item;
}

function json(value: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return Response.json(value, {
    status,
    headers: { ETag: '"record"', "Teslatlas-Protocol-Version": "1.2.0", ...headers },
  });
}

async function clientWith(
  response: FetchImplementation,
  authorization: AuthorizationProvider = () => "Bearer synthetic",
  requestedProtocolVersion: SupportedProtocolVersion = "1.2.0",
) {
  return new TeslatlasClient(
    await createClientSession({
      baseUrl: "https://synthetic.invalid",
      authorization,
      requestedProtocolVersion,
      fetch: async (input, init) =>
        new URL(String(input)).pathname === "/.well-known/teslatlas-hub"
          ? Response.json(discovery, { headers: { ETag: '"discovery"' } })
          : response(input, init),
    }),
  );
}

const operations = [
  ["read", (client: TeslatlasClient, signal: AbortSignal) => client.listVehicles({ signal })],
  [
    "create metadata",
    (client: TeslatlasClient, signal: AbortSignal) =>
      client.createMetadata(metadata.vehicle_id, metadataCreate, { signal }),
  ],
  [
    "replace metadata",
    (client: TeslatlasClient, signal: AbortSignal) =>
      client.replaceMetadata(metadata.metadata_id, { value: metadata.value }, { ifMatch, signal }),
  ],
  [
    "delete metadata",
    (client: TeslatlasClient, signal: AbortSignal) =>
      client.deleteMetadata(metadata.metadata_id, { ifMatch, signal }),
  ],
  [
    "command",
    (client: TeslatlasClient, signal: AbortSignal) =>
      client.createCommand(command as CommandRequest, { idempotencyKey, signal }),
  ],
] as const;

describe("TS-FULL-RICH-001 authorization cancellation", () => {
  it.each(operations)(
    "%s settles before pending authorization and never dispatches after late resolution/rejection",
    async (_name, invoke) => {
      for (const late of ["resolve", "reject"] as const) {
        const entered = pending<void>();
        const authorization = pending<string>();
        const fetch = vi.fn(async () => json(vehicles));
        const client = await clientWith(fetch, () => {
          entered.resolve();
          return authorization.promise;
        });
        const controller = new AbortController();
        const reason = new Error("caller cancellation");
        const outcome = invoke(client, controller.signal);
        const assertion = expect(outcome).rejects.toBe(reason);
        await entered.promise;
        controller.abort(reason);
        await assertion;
        if (late === "resolve") authorization.resolve("Bearer late");
        else authorization.reject(new Error("late provider rejection"));
        await authorization.promise.catch(() => undefined);
        await Promise.resolve();
        expect(fetch).not.toHaveBeenCalled();
      }
    },
  );

  it.each(operations)("pre-aborted %s skips authorization and Fetch", async (_name, invoke) => {
    const authorization = vi.fn(() => "Bearer synthetic");
    const fetch = vi.fn(async () => json(vehicles));
    const client = await clientWith(fetch, authorization);
    const controller = new AbortController();
    const reason = new Error("pre-aborted");
    controller.abort(reason);
    await expect(invoke(client, controller.signal)).rejects.toBe(reason);
    expect(authorization).not.toHaveBeenCalled();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("after Fetch command cancellation remains cause-free uncertainty after exactly one dispatch", async () => {
    const entered = pending<void>();
    const response = pending<Response>();
    const fetch = vi.fn(() => {
      entered.resolve();
      return response.promise;
    });
    const authorization = vi.fn(() => "Bearer synthetic");
    const client = await clientWith(fetch, authorization);
    const controller = new AbortController();
    const outcome = client
      .createCommand(command as CommandRequest, { idempotencyKey, signal: controller.signal })
      .catch((error: unknown) => error);
    await entered.promise;
    controller.abort(new Error("after dispatch"));
    response.reject(controller.signal.reason);
    const error = await outcome;
    expect(error).toMatchObject({ code: "command_uncertain" });
    expect(error).not.toHaveProperty("cause");
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(authorization).toHaveBeenCalledTimes(1);
  });
});

const reads = [
  [
    "current",
    current,
    "vehicle_id",
    (client: TeslatlasClient) => client.getVehicleCurrentState(current.vehicle_id),
  ],
  [
    "drive",
    drives.items[0],
    "drive_id",
    (client: TeslatlasClient) => client.getDrive(first(drives.items).drive_id),
  ],
  [
    "charge",
    charges.items[0],
    "charge_id",
    (client: TeslatlasClient) => client.getCharge(first(charges.items).charge_id),
  ],
  [
    "metadata",
    metadata,
    "metadata_id",
    (client: TeslatlasClient) => client.getMetadata(metadata.metadata_id),
  ],
  [
    "metadata tombstone",
    tombstone,
    "metadata_id",
    (client: TeslatlasClient) => client.getMetadata(tombstone.metadata_id),
  ],
  ["command", job, "command_id", (client: TeslatlasClient) => client.getCommand(job.command_id)],
  [
    "vehicle drives",
    drives,
    "vehicle_id",
    (client: TeslatlasClient) => client.listVehicleDrives(first(drives.items).vehicle_id),
  ],
  [
    "drive positions",
    positions,
    "drive_id",
    (client: TeslatlasClient) => client.listDrivePositions(first(positions.items).drive_id),
  ],
  [
    "vehicle charges",
    charges,
    "vehicle_id",
    (client: TeslatlasClient) => client.listVehicleCharges(first(charges.items).vehicle_id),
  ],
  [
    "charge samples",
    samples,
    "charge_id",
    (client: TeslatlasClient) => client.listChargeSamples(first(samples.items).charge_id),
  ],
  [
    "vehicle states",
    states,
    "vehicle_id",
    (client: TeslatlasClient) => client.listVehicleStates(first(states.items).vehicle_id),
  ],
  [
    "vehicle updates",
    updates,
    "vehicle_id",
    (client: TeslatlasClient) => client.listVehicleUpdates(first(updates.items).vehicle_id),
  ],
  [
    "vehicle metadata",
    metadataPage,
    "vehicle_id",
    (client: TeslatlasClient) => client.listVehicleMetadata(first(metadataPage.items).vehicle_id),
  ],
  [
    "filtered vehicle quality",
    {
      ...quality,
      items: [{ ...quality.items[0], subject_type: "vehicle", subject_id: current.vehicle_id }],
    },
    "subject_id",
    (client: TeslatlasClient) => client.listDataQuality({ vehicleId: current.vehicle_id }),
  ],
] as const;

describe("TS-FULL-RICH-004 resource identity", () => {
  it("preserves filtered indirect quality subjects without inventing absent ownership fields", async () => {
    const client = await clientWith(async () => json(quality));
    await expect(client.listDataQuality({ vehicleId: current.vehicle_id })).resolves.toMatchObject({
      value: quality,
    });
  });
  it.each(reads)(
    "%s accepts its matching identity and rejects a schema-valid different identity",
    async (_name, body, key, invoke) => {
      const matching = await clientWith(async () => json(body));
      await expect(invoke(matching)).resolves.toMatchObject({ kind: "modified" });
      const different = structuredClone(body) as unknown as Record<string, unknown>;
      if ("items" in different) {
        first(different.items as Record<string, unknown>[])[key] = "unrelated_resource_0001";
      } else different[key] = "unrelated_resource_0001";
      const mismatched = await clientWith(async () => json(different));
      await expect(invoke(mismatched)).rejects.toMatchObject({ code: "protocol_validation" });
    },
  );

  it.each(["create", "replace", "delete"] as const)(
    "metadata %s preserves valid writes and rejects misbound responses",
    async (operation) => {
      const body = operation === "delete" ? tombstone : metadata;
      const status = operation === "create" ? 201 : 200;
      const invoke = (client: TeslatlasClient) =>
        operation === "create"
          ? client.createMetadata(metadata.vehicle_id, metadataCreate)
          : operation === "replace"
            ? client.replaceMetadata(metadata.metadata_id, { value: metadata.value }, { ifMatch })
            : client.deleteMetadata(metadata.metadata_id, { ifMatch });
      const headers = { Location: `/v1/metadata/${metadata.metadata_id}` };
      const matching = await clientWith(async () => json(body, status, headers));
      await expect(invoke(matching)).resolves.toMatchObject({ metadata: { status } });
      const key = operation === "create" ? "vehicle_id" : "metadata_id";
      const mismatched = await clientWith(async () =>
        json({ ...body, [key]: "unrelated_resource_0001" }, status, headers),
      );
      await expect(invoke(mismatched)).rejects.toMatchObject({ code: "protocol_validation" });
    },
  );

  it.each(["vehicle_id", "command", "command_class", "location"] as const)(
    "command %s mismatch stays one-shot uncertainty",
    async (key) => {
      const body = {
        ...job,
        ...(key === "location"
          ? {}
          : {
              [key]:
                key === "command_class"
                  ? "nuisance"
                  : key === "command"
                    ? "honk_horn"
                    : "unrelated_resource_0001",
            }),
      };
      const fetch = vi.fn(async () =>
        json(body, 202, {
          Location:
            key === "location" ? "/v1/commands/another_job_0001" : `/v1/commands/${job.command_id}`,
        }),
      );
      const authorization = vi.fn(() => "Bearer synthetic");
      const client = await clientWith(fetch, authorization);
      const error = await client
        .createCommand(command as CommandRequest, { idempotencyKey })
        .catch((error: unknown) => error);
      expect(error).toMatchObject({ code: "command_uncertain" });
      expect(error).not.toHaveProperty("cause");
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(authorization).toHaveBeenCalledTimes(1);
    },
  );

  it("matching command acknowledgement succeeds", async () => {
    const client = await clientWith(async () =>
      json(job, 202, { Location: `/v1/commands/${job.command_id}` }),
    );
    await expect(
      client.createCommand(command as CommandRequest, { idempotencyKey }),
    ).resolves.toMatchObject({ value: job });
  });
});

describe("TS-FULL-RICH-005 discovery media type", () => {
  it.each([undefined, "text/plain"])("rejects %s and cancels unread body", async (contentType) => {
    const cancel = vi.fn();
    const body = new ReadableStream<Uint8Array>({ cancel });
    await expect(
      createClientSession({
        baseUrl: "https://synthetic.invalid",
        authorization: () => undefined,
        fetch: async () =>
          new Response(body, {
            headers: {
              ETag: '"discovery"',
              ...(contentType === undefined ? {} : { "Content-Type": contentType }),
            },
          }),
      }),
    ).rejects.toMatchObject({
      code: "protocol_validation",
      validator: "validateDiscovery.contentType",
    });
    expect(cancel).toHaveBeenCalledTimes(1);
  });

  it("accepts JSON media type with legal parameters and no version header", async () => {
    await expect(
      createClientSession({
        baseUrl: "https://synthetic.invalid",
        authorization: () => undefined,
        fetch: async () =>
          new Response(JSON.stringify(discovery), {
            headers: { ETag: '"discovery"', "Content-Type": "application/json; charset=utf-8" },
          }),
      }),
    ).resolves.toMatchObject({ protocolVersion: "1.2.0" });
  });
});

function notModified(stream = false): Response {
  const response = new Response(
    stream
      ? new ReadableStream({
          start(controller) {
            controller.close();
          },
        })
      : null,
    {
      status: stream ? 200 : 304,
      headers: { ETag: 'W/"cached"', "Teslatlas-Protocol-Version": "1.2.0" },
    },
  );
  if (stream) Object.defineProperty(response, "status", { value: 304 });
  return response;
}

describe("TS-FULL-RICH-006/007 conditional cancellation and binding", () => {
  it.each([false, true])(
    "null/stream %s rejects canceled response with matching validator",
    async (stream) => {
      const entered = pending<void>();
      const response = pending<Response>();
      const client = await clientWith(() => {
        entered.resolve();
        return response.promise;
      });
      const controller = new AbortController();
      const reason = new Error("cancel conditional");
      const result = client.listVehicles({
        ifNoneMatch: asEntityTag('"cached"'),
        signal: controller.signal,
      });
      const assertion = expect(result).rejects.toBe(reason);
      await entered.promise;
      controller.abort(reason);
      response.resolve(notModified(stream));
      await assertion;
    },
  );

  it.each([undefined, '"different"', '"cached"', 'W/"cached"'])(
    "304 validator context %s",
    async (sent) => {
      const client = await clientWith(async () => notModified(true));
      const result = client.listVehicles(
        sent === undefined ? {} : { ifNoneMatch: asEntityTag(sent) },
      );
      if (sent === undefined || sent === '"different"')
        await expect(result).rejects.toMatchObject({ validator: "validateVehiclePage.304.etag" });
      else await expect(result).resolves.toMatchObject({ kind: "not-modified" });
    },
  );

  it("rejects nonempty 304 even with matching validator", async () => {
    const response = new Response("unexpected", {
      headers: { ETag: '"cached"', "Teslatlas-Protocol-Version": "1.2.0" },
    });
    Object.defineProperty(response, "status", { value: 304 });
    const client = await clientWith(async () => response);
    await expect(
      client.listVehicles({ ifNoneMatch: asEntityTag('"cached"') }),
    ).rejects.toMatchObject({ validator: "validateVehiclePage.304" });
  });
});

describe("TS-FULL-RICH-008 selected response version", () => {
  it.each(["1.0.0", "1.1.0", "1.2.0"])(
    "accepts advertised supported selection %s <= requested",
    async (version) => {
      const client = await clientWith(async () =>
        json(current, 200, { "Teslatlas-Protocol-Version": version }),
      );
      await expect(client.getVehicleCurrentState(current.vehicle_id)).resolves.toMatchObject({
        metadata: { protocolVersion: version },
      });
    },
  );

  it.each([undefined, "9.9.9", "1.3.0", "01.2.0", "1.2.1"])(
    "rejects missing/unsupported/malformed version %s before body consumption",
    async (version) => {
      const cancel = vi.fn();
      const response = new Response(new ReadableStream({ cancel }), {
        headers: {
          ETag: '"current"',
          "Content-Type": "application/json",
          ...(version === undefined ? {} : { "Teslatlas-Protocol-Version": version }),
        },
      });
      const client = await clientWith(async () => response);
      await expect(client.getVehicleCurrentState(current.vehicle_id)).rejects.toMatchObject({
        validator: "validateCurrentState.protocolVersion",
      });
      expect(cancel).toHaveBeenCalledTimes(1);
    },
  );

  it("rejects a supported version newer than the request", async () => {
    const client = await clientWith(async () => json(current), undefined, "1.1.0");
    await expect(client.getVehicleCurrentState(current.vehicle_id)).rejects.toMatchObject({
      validator: "validateCurrentState.protocolVersion",
    });
  });
});
