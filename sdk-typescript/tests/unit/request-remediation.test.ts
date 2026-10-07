import { Ajv2020 } from "ajv/dist/2020.js";
import { describe, expect, it, vi } from "vitest";
import commandJob from "../../protocol/source/examples/command-job.json" with { type: "json" };
import commandRequestJson from "../../protocol/source/examples/command-request.json" with {
  type: "json",
};
import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import metadataRecord from "../../protocol/source/examples/metadata-record.json" with {
  type: "json",
};
import { TeslatlasClient } from "../../src/client/client.js";
import type { ClientSession } from "../../src/client/types.js";
import { validateAdvertisedCommand } from "../../src/commands/advertised-command.js";
import { asIdempotencyKey } from "../../src/commands/idempotency.js";
import { CommandUncertainError, ProtocolValidationError } from "../../src/core/errors.js";
import { FetchTransport } from "../../src/http/fetch-transport.js";
import { InvalidRequestBodyError } from "../../src/http/request-builder.js";
import { asStrongEntityTag, InvalidStrongEntityTagError } from "../../src/http/strong-etag.js";
import type {
  CommandRequest,
  HubDescriptor,
  MetadataCreate,
  MetadataReplace,
} from "../../src/protocol/models.js";

const idempotencyKey = asIdempotencyKey("11111111-1111-4111-8111-111111111111");
const commandRequest = commandRequestJson as unknown as CommandRequest;
const metadataCreate = {
  vehicle_id: "vehicle_demo_alpha",
  kind: "note",
  target: { resource_type: "drive", resource_id: "drive_demo_0001" },
  value: { text: "redacted" },
} as unknown as MetadataCreate;

describe("request admission remediation", () => {
  it("validates the command after an idempotency option getter changes its parameters", async () => {
    const body = structuredClone(commandRequestJson);
    const state = createClient();
    let optionReads = 0;

    await expect(
      state.client.createCommand(body as unknown as CommandRequest, {
        get idempotencyKey() {
          optionReads += 1;
          body.parameters.percent = 20;
          return idempotencyKey;
        },
      }),
    ).rejects.toMatchObject({ code: "protocol_validation", validator: "command.descriptor" });

    expect(optionReads).toBe(1);
    expect(state.authorization).not.toHaveBeenCalled();
    expect(state.fetch).not.toHaveBeenCalled();
  });

  it("dispatches only the admitted command snapshot when a Proxy changes later reads", async () => {
    let parameterReads = 0;
    const parameters = new Proxy(
      { percent: 80 },
      {
        getOwnPropertyDescriptor(target, key) {
          const descriptor = Reflect.getOwnPropertyDescriptor(target, key);
          if (key === "percent" && descriptor !== undefined) {
            parameterReads += 1;
            target.percent = 20;
            return descriptor;
          }
          return descriptor;
        },
        get(target, key, receiver) {
          return Reflect.get(target, key, receiver);
        },
      },
    );
    const state = createClient();
    const body = { ...commandRequest, parameters };

    await expect(
      state.client.createCommand(body as unknown as CommandRequest, { idempotencyKey }),
    ).resolves.toMatchObject({
      metadata: { status: 202 },
    });

    expect(parameterReads).toBe(1);
    expect(state.fetch).toHaveBeenCalledTimes(1);
    const sent = JSON.parse(state.bodies[0] ?? "null") as CommandRequest;
    expect(sent.parameters).toEqual({ percent: 80 });
    expect(() => validateAdvertisedCommand(state.descriptor, sent)).not.toThrow();
  });

  it("validates the metadata snapshot after a signal getter changes its vehicle binding", async () => {
    const body = structuredClone(metadataCreate);
    const state = createClient();

    await expect(
      state.client.createMetadata("vehicle_demo_alpha", body, {
        get signal() {
          body.vehicle_id = "vehicle_demo_other";
          return new AbortController().signal;
        },
      }),
    ).rejects.toMatchObject({ validator: "validateMetadataCreate.vehicle_id" });
    expect(state.authorization).not.toHaveBeenCalled();
    expect(state.fetch).not.toHaveBeenCalled();
  });

  it.each(["command", "create", "replace"] as const)(
    "rejects an already-aborted %s before inspecting the body or compiling schemas",
    async (operation) => {
      const reason = new Error("caller cancellation");
      const controller = new AbortController();
      controller.abort(reason);
      const state = createClient();
      const bodyInspections = vi.fn(() => {
        throw new Error("body must remain untouched");
      });
      const body = new Proxy({}, { getPrototypeOf: bodyInspections });
      const compile = vi.spyOn(Ajv2020.prototype, "compile");
      try {
        const result =
          operation === "command"
            ? state.client.createCommand(body as CommandRequest, {
                idempotencyKey,
                signal: controller.signal,
              })
            : operation === "create"
              ? state.client.createMetadata("vehicle_demo_alpha", body as MetadataCreate, {
                  signal: controller.signal,
                })
              : state.client.replaceMetadata("metadata_demo_note_0001", body as MetadataReplace, {
                  ifMatch: asStrongEntityTag('"metadata-1"'),
                  signal: controller.signal,
                });
        await expect(result).rejects.toBe(reason);
        expect(bodyInspections).not.toHaveBeenCalled();
        expect(compile).not.toHaveBeenCalled();
        expect(state.authorization).not.toHaveBeenCalled();
        expect(state.fetch).not.toHaveBeenCalled();
      } finally {
        compile.mockRestore();
      }
    },
  );

  it.each(["command", "create", "replace"] as const)(
    "counts exact UTF-8 bytes for a %s at the discovered budget plus or minus one",
    async (operation) => {
      const bodies: Record<typeof operation, unknown> = {
        command: {
          ...commandRequest,
          parameters: { percent: 80, memo: "é".repeat(800) },
        },
        create: { ...metadataCreate, value: { text: "é".repeat(800) } },
        replace: { value: { text: "é".repeat(800) } },
      };
      const body = bodies[operation];
      const serialized = JSON.stringify(body);
      const bytes = new TextEncoder().encode(serialized).byteLength;
      expect(bytes).toBeGreaterThan(serialized.length);
      expect(bytes).toBeGreaterThan(1_024);
      for (const difference of [-1, 0, 1]) {
        const state = createClient(bytes + difference);
        const advertised = state.descriptor.capabilities.find(({ id }) => id === "commands.async")
          ?.commands?.[0];
        if (advertised !== undefined) {
          (advertised.parameters_schema as Record<string, unknown>).additionalProperties = true;
        }
        const result =
          operation === "command"
            ? state.client.createCommand(body as CommandRequest, { idempotencyKey })
            : operation === "create"
              ? state.client.createMetadata("vehicle_demo_alpha", body as MetadataCreate)
              : state.client.replaceMetadata("metadata_demo_note_0001", body as MetadataReplace, {
                  ifMatch: asStrongEntityTag('"metadata-1"'),
                });
        if (difference === -1) {
          await expect(result).rejects.toMatchObject({
            code: "protocol_validation",
            validator: "request.body.size",
          });
          expect(state.authorization).not.toHaveBeenCalled();
          expect(state.fetch).not.toHaveBeenCalled();
        } else {
          await expect(result).resolves.toBeDefined();
          expect(state.authorization).toHaveBeenCalledTimes(1);
          expect(state.fetch).toHaveBeenCalledTimes(1);
          expect(state.bodies).toEqual([serialized]);
        }
      }
    },
  );

  it.each([
    {
      value: {
        get text() {
          return "hidden getter";
        },
      },
    },
    {
      value: {
        toJSON() {
          return { text: "replaced" };
        },
      },
    },
    { value: { text: undefined } },
    { value: { text: Number.POSITIVE_INFINITY } },
    { value: Array(1) },
  ])("keeps lossy and executable JSON bodies outside authorization", async (replace) => {
    const state = createClient();

    await expect(
      state.client.createMetadata("vehicle_demo_alpha", {
        ...metadataCreate,
        ...replace,
      } as MetadataCreate),
    ).rejects.toBeInstanceOf(InvalidRequestBodyError);
    expect(state.authorization).not.toHaveBeenCalled();
    expect(state.fetch).not.toHaveBeenCalled();
  });

  it("keeps a genuine server 413 typed after one command dispatch", async () => {
    const state = createClient(undefined, () =>
      Response.json(
        {
          type: "urn:teslatlas:problem:request-too-large",
          title: "Request too large",
          status: 413,
          instance: "/requests/request_body_limit",
          code: "request_too_large",
          request_id: "request_body_limit",
          retryable: false,
        },
        { status: 413, headers: { "Content-Type": "application/problem+json" } },
      ),
    );

    await expect(
      state.client.createCommand(commandRequest, { idempotencyKey }),
    ).rejects.toMatchObject({
      name: "ProtocolHttpError",
      status: 413,
      code: "request_too_large",
    });
    expect(state.fetch).toHaveBeenCalledTimes(1);
  });

  it("accepts a long rich response ETag and rejects its oversized mutation precondition locally", async () => {
    const etag = `"${"x".repeat(512)}"`;
    const state = createClient(undefined, () =>
      Response.json(metadataRecord, {
        headers: { ETag: etag, "Teslatlas-Protocol-Version": "1.2.0" },
      }),
    );
    const read = await state.client.getMetadata("metadata_demo_note_0001");
    expect(read.metadata.etag).toBe(etag);
    const responseTag = read.metadata.etag;
    if (responseTag === undefined) throw new Error("Expected returned metadata ETag");
    expect(() => asStrongEntityTag(responseTag)).toThrow(InvalidStrongEntityTagError);

    // JavaScript callers can pass a returned tag without constructing a brand.
    // Exercise both real method boundaries without fabricating StrongEntityTag.
    await expect(
      Reflect.apply(state.client.replaceMetadata, state.client, [
        "metadata_demo_note_0001",
        {
          value: { text: "redacted update" },
        },
        { ifMatch: responseTag },
      ]),
    ).rejects.toBeInstanceOf(InvalidStrongEntityTagError);
    await expect(
      Reflect.apply(state.client.deleteMetadata, state.client, [
        "metadata_demo_note_0001",
        { ifMatch: responseTag },
      ]),
    ).rejects.toBeInstanceOf(InvalidStrongEntityTagError);
    expect(state.authorization).toHaveBeenCalledTimes(1);
    expect(state.fetch).toHaveBeenCalledTimes(1);
    expect(state.bodies).toEqual([]);
  });

  it("retains cause-free one-shot uncertainty after a snapshotted command dispatch", async () => {
    const state = createClient(undefined, () => {
      throw new Error("private transport detail");
    });

    const error = await state.client
      .createCommand(commandRequest, { idempotencyKey })
      .catch((caught: unknown) => caught);

    expect(error).toBeInstanceOf(CommandUncertainError);
    expect(error).not.toHaveProperty("cause");
    expect(String(error)).not.toContain("private");
    expect(state.authorization).toHaveBeenCalledTimes(1);
    expect(state.fetch).toHaveBeenCalledTimes(1);
  });
});

describe("advertised command validator cache", () => {
  it("reuses both compiled validators and invalidates nested schema edits", () => {
    const descriptor = structuredClone(discovery) as HubDescriptor;
    const advertised = descriptor.capabilities.find(({ id }) => id === "commands.async")
      ?.commands?.[0];
    if (advertised === undefined) throw new Error("Missing command fixture");
    const parameters = advertised.parameters_schema as Record<string, unknown>;
    const expected = advertised.expected_state_schema as Record<string, unknown>;
    parameters.title = "cache-regression-parameters";
    expected.title = "cache-regression-state";
    const compile = vi.spyOn(Ajv2020.prototype, "compile");
    const count = () =>
      compile.mock.calls.filter(
        ([schema]) =>
          schema !== null &&
          typeof schema === "object" &&
          "title" in schema &&
          typeof schema.title === "string" &&
          schema.title.startsWith("cache-regression-"),
      ).length;
    try {
      validateAdvertisedCommand(descriptor, commandRequest);
      validateAdvertisedCommand(descriptor, commandRequest);
      expect(count()).toBe(2);

      const percent = (parameters.properties as Record<string, Record<string, unknown>>).percent;
      if (percent === undefined) throw new Error("Missing percent schema");
      percent.minimum = 90;
      expect(() => validateAdvertisedCommand(descriptor, commandRequest)).toThrow(
        ProtocolValidationError,
      );
      expect(count()).toBe(3);
      percent.minimum = 50;
      validateAdvertisedCommand(descriptor, commandRequest);
      expect(count()).toBe(4);

      const chargeLimit = (expected.properties as Record<string, Record<string, unknown>>)
        .charge_limit_percent;
      if (chargeLimit === undefined) throw new Error("Missing expected-state schema");
      chargeLimit.minimum = 90;
      expect(() => validateAdvertisedCommand(descriptor, commandRequest)).toThrow(
        ProtocolValidationError,
      );
      expect(count()).toBe(5);
      expected.$async = true;
      expect(() => validateAdvertisedCommand(descriptor, commandRequest)).toThrow(
        ProtocolValidationError,
      );
      expect(count()).toBe(5);
    } finally {
      compile.mockRestore();
    }
  });

  it("does not reuse a replaced schema or skip changed descriptor semantics", () => {
    const descriptor = structuredClone(discovery) as HubDescriptor;
    const advertised = descriptor.capabilities.find(({ id }) => id === "commands.async")
      ?.commands?.[0];
    if (advertised === undefined) throw new Error("Missing command fixture");
    validateAdvertisedCommand(descriptor, commandRequest);
    const replacement = {
      ...advertised,
      parameters_schema: { ...advertised.parameters_schema, required: ["unavailable"] },
    };
    const capability = descriptor.capabilities.find(({ id }) => id === "commands.async");
    if (capability?.commands === undefined) throw new Error("Missing command capability");
    capability.commands[0] = replacement;
    expect(() => validateAdvertisedCommand(descriptor, commandRequest)).toThrow(
      ProtocolValidationError,
    );
    capability.commands[0] = { ...advertised, command_class: "climate" };
    expect(() => validateAdvertisedCommand(descriptor, commandRequest)).toThrow(
      ProtocolValidationError,
    );
  });
});

function createClient(maximumBytes?: number, respond?: () => Response) {
  const descriptor = structuredClone(discovery) as HubDescriptor;
  if (maximumBytes !== undefined) descriptor.limits.max_request_body_bytes = maximumBytes;
  const bodies: string[] = [];
  const authorization = vi.fn(() => "Bearer caller-owned");
  const fetch = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
    if (typeof init?.body === "string") bodies.push(init.body);
    if (respond !== undefined) return respond();
    if (new Headers(init?.headers).has("Idempotency-Key")) {
      return Response.json(commandJob, {
        status: 202,
        headers: {
          ETag: '"command-1"',
          Location: "/v1/commands/command_demo_0001",
          "Teslatlas-Protocol-Version": "1.2.0",
        },
      });
    }
    return Response.json(metadataRecord, {
      status: init?.method === "POST" ? 201 : 200,
      headers: {
        ETag: '"metadata-1"',
        Location: "/v1/metadata/metadata_demo_note_0001",
        "Teslatlas-Protocol-Version": "1.2.0",
      },
    });
  });
  const session: ClientSession = {
    descriptor,
    protocolVersion: "1.2.0",
    discoveryTransport: new FetchTransport({ baseUrl: "https://hub.example.invalid", fetch }),
    apiTransport: new FetchTransport({
      baseUrl: "https://api.example.invalid",
      authorization,
      fetch,
    }),
    eventTransport: new FetchTransport({ baseUrl: "https://events.example.invalid", fetch }),
  };
  return { client: new TeslatlasClient(session), descriptor, authorization, fetch, bodies };
}
