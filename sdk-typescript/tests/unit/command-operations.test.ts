import commandJob from "../../protocol/source/examples/command-job.json" with { type: "json" };
import commandRequestJson from "../../protocol/source/examples/command-request.json" with {
  type: "json",
};
import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import { describe, expect, it } from "vitest";
import { asIdempotencyKey, type IdempotencyKey } from "../../src/commands/idempotency.js";
import { TeslatlasClient } from "../../src/client/client.js";
import type { ClientSession } from "../../src/client/types.js";
import {
  CommandUncertainError,
  MissingCapabilityError,
  ProtocolValidationError,
} from "../../src/core/errors.js";
import { asEntityTag } from "../../src/core/opaque-values.js";
import { validateCommandRequest, validateDiscovery } from "../../src/generated/validators.js";
import { FetchTransport, type FetchImplementation } from "../../src/http/fetch-transport.js";
import type { CommandRequest, HubDescriptor } from "../../src/protocol/models.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";

const descriptor = decodeProtocolValue<HubDescriptor>(discovery, validateDiscovery, "discovery");
const commandRequest = decodeProtocolValue<CommandRequest>(
  commandRequestJson,
  validateCommandRequest,
  "validateCommandRequest",
);

interface ObservedRequest {
  readonly method: string;
  readonly path: string;
  readonly headers: Headers;
  readonly body: string | null;
  readonly redirect: RequestRedirect | undefined;
}

describe("typed command operations", () => {
  it("posts one validated command and reads its fixed status route", async () => {
    const observed: ObservedRequest[] = [];
    const client = createClient(observed, async (input, init) => {
      const request = observe(input, init, observed);
      if (request.method === "POST") {
        return Response.json(commandJob, {
          status: 202,
          headers: {
            ETag: 'W/"command-1"',
            Location: "/v1/commands/command_demo_0001",
          },
        });
      }
      return Response.json(commandJob, { headers: { ETag: 'W/"command-1"' } });
    });

    const created = await client.createCommand(commandRequest, {
      idempotencyKey: asIdempotencyKey("11111111-1111-4111-8111-111111111111"),
    });
    const read = await client.getCommand("command/demo", {
      ifNoneMatch: asEntityTag('W/"command-1"'),
    });

    expect(created).toMatchObject({
      value: { command_id: "command_demo_0001" },
      metadata: {
        status: 202,
        etag: 'W/"command-1"',
        location: "/v1/commands/command_demo_0001",
      },
    });
    expect(read).toMatchObject({ kind: "modified", value: { command_id: "command_demo_0001" } });
    expect(
      observed.map((request) => ({
        method: request.method,
        path: request.path,
        authorization: request.headers.get("authorization"),
        protocolVersion: request.headers.get("teslatlas-protocol-version"),
        idempotencyKey: request.headers.get("idempotency-key"),
        ifNoneMatch: request.headers.get("if-none-match"),
        contentType: request.headers.get("content-type"),
        body: request.body,
        redirect: request.redirect,
      })),
    ).toEqual([
      {
        method: "POST",
        path: "/v1/commands",
        authorization: "Bearer caller-owned",
        protocolVersion: "1.2.0",
        idempotencyKey: "11111111-1111-4111-8111-111111111111",
        ifNoneMatch: null,
        contentType: "application/json",
        body: JSON.stringify(commandRequest),
        redirect: "error",
      },
      {
        method: "GET",
        path: "/v1/commands/command%2Fdemo",
        authorization: "Bearer caller-owned",
        protocolVersion: "1.2.0",
        idempotencyKey: null,
        ifNoneMatch: 'W/"command-1"',
        contentType: null,
        body: null,
        redirect: "error",
      },
    ]);
  });

  it("preflights malformed command data, descriptor semantics, keys, and capability", async () => {
    let authorizationCalls = 0;
    let fetchCalls = 0;
    const fetch: FetchImplementation = async () => {
      fetchCalls += 1;
      return Response.json(commandJob);
    };
    const authorization = () => {
      authorizationCalls += 1;
      return "Bearer caller-owned";
    };
    const client = createClient([], fetch, descriptor, authorization);
    const noCommandCapability: HubDescriptor = {
      ...descriptor,
      capabilities: descriptor.capabilities.filter(({ id }) => id !== "commands.async"),
    };
    const noCapabilityClient = createClient([], fetch, noCommandCapability, authorization);

    await expect(
      client.createCommand(commandRequest, { idempotencyKey: "not-a-uuid" as IdempotencyKey }),
    ).rejects.toMatchObject({ code: "invalid_idempotency_key" });
    await expect(
      client.createCommand({ ...commandRequest, parameters: { percent: 20 } } as CommandRequest, {
        idempotencyKey: asIdempotencyKey("11111111-1111-4111-8111-111111111111"),
      }),
    ).rejects.toBeInstanceOf(ProtocolValidationError);
    await expect(
      client.createCommand({ ...commandRequest, confirmation: undefined } as CommandRequest, {
        idempotencyKey: asIdempotencyKey("22222222-2222-4222-8222-222222222222"),
      }),
    ).rejects.toBeInstanceOf(ProtocolValidationError);
    await expect(
      client.createCommand({ ...commandRequest, command_class: "climate" } as CommandRequest, {
        idempotencyKey: asIdempotencyKey("33333333-3333-4333-8333-333333333333"),
      }),
    ).rejects.toBeInstanceOf(ProtocolValidationError);
    await expect(
      noCapabilityClient.createCommand(commandRequest, {
        idempotencyKey: asIdempotencyKey("44444444-4444-4444-8444-444444444444"),
      }),
    ).rejects.toBeInstanceOf(MissingCapabilityError);

    expect(authorizationCalls).toBe(0);
    expect(fetchCalls).toBe(0);
  });

  it.each([
    [429, "rate_limited"],
    [503, "unavailable"],
  ])("keeps a conforming %i command problem typed after one fetch", async (status, code) => {
    let fetchCalls = 0;
    const client = createClient([], async () => {
      fetchCalls += 1;
      return Response.json(problem(status, code), {
        status,
        headers: { "Content-Type": "application/problem+json" },
      });
    });

    await expect(
      client.createCommand(commandRequest, {
        idempotencyKey: asIdempotencyKey("55555555-5555-4555-8555-555555555555"),
      }),
    ).rejects.toMatchObject({ name: "ProtocolHttpError", status, code });
    expect(fetchCalls).toBe(1);
  });

  it.each(["transport", "abort"] as const)(
    "turns a post-dispatch %s into a cause-free uncertainty after one fetch",
    async (kind) => {
      let fetchCalls = 0;
      const controller = new AbortController();
      const client = createClient([], async () => {
        fetchCalls += 1;
        if (kind === "abort") {
          controller.abort(new Error("caller abort secret"));
          throw controller.signal.reason;
        }
        throw new Error("transport secret");
      });

      const error = await captureError(
        client.createCommand(commandRequest, {
          idempotencyKey: asIdempotencyKey("66666666-6666-4666-8666-666666666666"),
          ...(kind === "abort" ? { signal: controller.signal } : {}),
        }),
      );

      expect(error).toBeInstanceOf(CommandUncertainError);
      expect(error).not.toHaveProperty("cause");
      expect(String(error)).not.toContain("secret");
      expect(fetchCalls).toBe(1);
    },
  );

  it("turns an abort while reading a command receipt into a cause-free uncertainty", async () => {
    let fetchCalls = 0;
    const controller = new AbortController();
    const reason = new Error("receipt abort secret");
    const client = createClient([], async () => {
      fetchCalls += 1;
      const body = new ReadableStream({
        start(stream) {
          queueMicrotask(() => {
            controller.abort(reason);
            stream.error(reason);
          });
        },
      });
      return new Response(body, {
        status: 202,
        headers: {
          "Content-Type": "application/json",
          ETag: '"command-1"',
          Location: "/v1/commands/command_demo_0001",
        },
      });
    });

    const error = await captureError(
      client.createCommand(commandRequest, {
        idempotencyKey: asIdempotencyKey("99999999-9999-4999-8999-999999999999"),
        signal: controller.signal,
      }),
    );

    expect(error).toBeInstanceOf(CommandUncertainError);
    expect(error).not.toHaveProperty("cause");
    expect(String(error)).not.toContain("secret");
    expect(fetchCalls).toBe(1);
  });

  it("honors an already-aborted command signal before authorization and Fetch", async () => {
    let authorizationCalls = 0;
    let fetchCalls = 0;
    const signal = new AbortController();
    const reason = new Error("caller cancellation");
    signal.abort(reason);
    const client = createClient(
      [],
      async () => {
        fetchCalls += 1;
        return Response.json(commandJob);
      },
      descriptor,
      () => {
        authorizationCalls += 1;
        return "Bearer caller-owned";
      },
    );

    await expect(
      client.createCommand(commandRequest, {
        idempotencyKey: asIdempotencyKey("77777777-7777-4777-8777-777777777777"),
        signal: signal.signal,
      }),
    ).rejects.toBe(reason);
    expect(authorizationCalls).toBe(0);
    expect(fetchCalls).toBe(0);
  });

  it("turns a malformed post-dispatch command body into cause-free uncertainty", async () => {
    const client = createClient(
      [],
      async () =>
        new Response("not-json", {
          status: 202,
          headers: {
            "Content-Type": "application/json",
            ETag: '"command-1"',
            Location: "/v1/commands/command_demo_0001",
          },
        }),
    );

    const error = await captureError(
      client.createCommand(commandRequest, {
        idempotencyKey: asIdempotencyKey("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
      }),
    );

    expect(error).toBeInstanceOf(CommandUncertainError);
    expect(error).not.toHaveProperty("cause");
  });

  it.each([
    [undefined, "/v1/commands/command_demo_0001"],
    ['W/"command-1"', undefined],
    ['W/"command-1"', "https://caller:secret@other.example.invalid/v1/commands/command_demo_0001"],
  ])("turns an invalid post-dispatch job receipt into uncertainty", async (etag, location) => {
    const client = createClient([], async () =>
      Response.json(commandJob, {
        status: 202,
        headers: {
          ...(etag === undefined ? {} : { ETag: etag }),
          ...(location === undefined ? {} : { Location: location }),
        },
      }),
    );

    await expect(
      client.createCommand(commandRequest, {
        idempotencyKey: asIdempotencyKey("88888888-8888-4888-8888-888888888888"),
      }),
    ).rejects.toBeInstanceOf(CommandUncertainError);
  });
});

function createClient(
  observed: ObservedRequest[],
  fetch: FetchImplementation = async (input, init) => {
    const request = observe(input, init, observed);
    throw new Error(`Unhandled request ${request.method} ${request.path}`);
  },
  sessionDescriptor: HubDescriptor = descriptor,
  authorization: () => string = () => "Bearer caller-owned",
): TeslatlasClient {
  const session: ClientSession = {
    descriptor: sessionDescriptor,
    protocolVersion: "1.2.0",
    discoveryTransport: new FetchTransport({ baseUrl: "https://hub.example.invalid", fetch }),
    apiTransport: new FetchTransport({
      baseUrl: "https://api.example.invalid",
      authorization,
      fetch,
    }),
    eventTransport: new FetchTransport({ baseUrl: "https://events.example.invalid", fetch }),
  };
  return new TeslatlasClient(session);
}

function observe(
  input: RequestInfo | URL,
  init: RequestInit | undefined,
  observed: ObservedRequest[],
): ObservedRequest {
  const url = new URL(String(input));
  const request = {
    method: init?.method ?? "GET",
    path: `${url.pathname}${url.search}`,
    headers: new Headers(init?.headers),
    body: typeof init?.body === "string" ? init.body : null,
    redirect: init?.redirect,
  };
  observed.push(request);
  return request;
}

function problem(status: number, code: string): unknown {
  return {
    type: `urn:teslatlas:problem:${code.replaceAll("_", "-")}`,
    title: "Command problem",
    status,
    instance: "/requests/request_command_problem",
    code,
    request_id: "request_command_problem",
    retryable: status >= 500 || status === 429,
  };
}

async function captureError(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
    return undefined;
  } catch (error) {
    return error;
  }
}
