import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import { describe, expect, it } from "vitest";
import { TeslatlasClient } from "../../src/client/client.js";
import type { ClientSession } from "../../src/client/types.js";
import { TransportError } from "../../src/core/errors.js";
import { subscribeToSse, type SseSleep } from "../../src/events/sse-subscription.js";
import { FetchTransport, type FetchImplementation } from "../../src/http/fetch-transport.js";
import { validateDiscovery } from "../../src/generated/validators.js";
import type { HubDescriptor, ProtocolEvent } from "../../src/protocol/models.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";

const descriptor = decodeProtocolValue<HubDescriptor>(discovery, validateDiscovery, "discovery");

interface EventClient {
  streamEvents(options?: {
    readonly sleep?: SseSleep;
    readonly signal?: AbortSignal;
  }): AsyncIterable<ProtocolEvent>;
}

describe("typed SSE reconnect policy", () => {
  it("uses the protocol default 3,000ms delay after accidental EOF", async () => {
    const waits: number[] = [];
    const responses = [eventStream(""), new Response(null, { status: 204 })];
    const client = createClient(async () => {
      const response = responses.shift();
      if (response === undefined) throw new Error("unexpected Fetch");
      return response;
    });

    await expect(collect(streamEvents(client, { sleep: recordSleep(waits) }))).resolves.toEqual([]);
    expect(waits).toEqual([3_000]);
  });

  it("uses one valid server retry hint and caps it at 30,000ms", async () => {
    const waits: number[] = [];
    const responses = [eventStream("retry: 50000\n\n"), new Response(null, { status: 204 })];
    const client = createClient(async () => {
      const response = responses.shift();
      if (response === undefined) throw new Error("unexpected Fetch");
      return response;
    });

    await expect(collect(streamEvents(client, { sleep: recordSleep(waits) }))).resolves.toEqual([]);
    expect(waits).toEqual([30_000]);
  });

  it("reconnects a safe transport failure but never an aborted stream", async () => {
    const waits: number[] = [];
    let calls = 0;
    const client = createClient(async () => {
      calls += 1;
      if (calls === 1) throw new Error("network unavailable");
      return new Response(null, { status: 204 });
    });

    await expect(collect(streamEvents(client, { sleep: recordSleep(waits) }))).resolves.toEqual([]);
    expect(calls).toBe(2);
    expect(waits).toEqual([3_000]);

    const controller = new AbortController();
    const reason = new DOMException("Stopped", "AbortError");
    let abortCalls = 0;
    const abortClient = createClient(async () => {
      abortCalls += 1;
      return eventStream("");
    });
    const abortSleep: SseSleep = async () => {
      controller.abort(reason);
      throw reason;
    };

    await expect(
      collect(streamEvents(abortClient, { signal: controller.signal, sleep: abortSleep })),
    ).rejects.toBe(reason);
    expect(abortCalls).toBe(1);
  });

  it("normalizes a mid-body transport rejection before the default reconnect", async () => {
    const waits: number[] = [];
    let calls = 0;
    const client = createClient(async () => {
      calls += 1;
      if (calls === 1) return failedEventStream(new Error("Bearer reader-secret"));
      return new Response(null, { status: 204 });
    });

    await expect(collect(streamEvents(client, { sleep: recordSleep(waits) }))).resolves.toEqual([]);
    expect(calls).toBe(2);
    expect(waits).toEqual([3_000]);
  });

  it("preserves an abort raised during a stream read without a later Fetch", async () => {
    const controller = new AbortController();
    const reason = new DOMException("Stopped", "AbortError");
    let calls = 0;
    const client = createClient(async () => {
      calls += 1;
      return failedEventStream(new Error("Bearer reader-secret"), () => controller.abort(reason));
    });

    await expect(collect(streamEvents(client, { signal: controller.signal }))).rejects.toBe(reason);
    expect(calls).toBe(1);
  });

  it("never exposes a raw mid-body failure when reconnect is declined", async () => {
    const transport = new FetchTransport({
      baseUrl: "https://events.example.invalid",
      fetch: async () => failedEventStream(new Error("Bearer reader-secret")),
    });
    const iterator = subscribeToSse({
      transport,
      path: "/v1/events",
      reconnect: () => undefined,
    })[Symbol.asyncIterator]();

    const error = await nextError(iterator);
    expect(error).toBeInstanceOf(TransportError);
    expect(error).not.toHaveProperty("cause");
    expect(String(error)).not.toContain("reader-secret");
  });

  it("normalizes a locked response body reader setup failure when reconnect is declined", async () => {
    let calls = 0;
    const transport = new FetchTransport({
      baseUrl: "https://events.example.invalid",
      fetch: async () => {
        calls += 1;
        return lockedEventStream();
      },
    });
    const iterator = subscribeToSse({
      transport,
      path: "/v1/events",
      reconnect: () => undefined,
    })[Symbol.asyncIterator]();

    const error = await nextError(iterator);

    expect(error).toBeInstanceOf(TransportError);
    expect(error).not.toHaveProperty("cause");
    expect(calls).toBe(1);
  });
});

function streamEvents(
  client: TeslatlasClient,
  options: { readonly sleep?: SseSleep; readonly signal?: AbortSignal } = {},
): AsyncIterable<ProtocolEvent> {
  return (client as unknown as EventClient).streamEvents(options);
}

function createClient(fetch: FetchImplementation): TeslatlasClient {
  const authorization = () => "Bearer caller-owned";
  const session: ClientSession = {
    descriptor,
    protocolVersion: "1.2.0",
    discoveryTransport: new FetchTransport({ baseUrl: "https://hub.example.invalid", fetch }),
    apiTransport: new FetchTransport({
      baseUrl: "https://api.example.invalid",
      authorization,
      fetch,
    }),
    eventTransport: new FetchTransport({
      baseUrl: "https://events.example.invalid",
      authorization,
      fetch,
    }),
  };
  return new TeslatlasClient(session);
}

function eventStream(wire: string): Response {
  return new Response(wire, { headers: { "Content-Type": "text/event-stream" } });
}

function failedEventStream(error: unknown, beforeError?: () => void): Response {
  return new Response(
    new ReadableStream<Uint8Array>({
      pull(controller) {
        beforeError?.();
        controller.error(error);
      },
    }),
    { headers: { "Content-Type": "text/event-stream" } },
  );
}

function lockedEventStream(): Response {
  const response = eventStream("data: ignored\n\n");
  response.body?.getReader();
  return response;
}

function recordSleep(waits: number[]): SseSleep {
  return async (milliseconds) => {
    waits.push(milliseconds);
  };
}

async function collect<T>(events: AsyncIterable<T>): Promise<T[]> {
  const result: T[] = [];
  for await (const event of events) result.push(event);
  return result;
}

async function nextError(iterator: AsyncIterator<unknown>): Promise<unknown> {
  try {
    await iterator.next();
    return undefined;
  } catch (error) {
    return error;
  }
}
