import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import metadataRecord from "../../protocol/source/examples/metadata-record.json" with {
  type: "json",
};
import { describe, expect, it } from "vitest";
import type { ClientSession } from "../../src/client/types.js";
import { streamProtocolEvents } from "../../src/events/protocol-subscription.js";
import {
  InvalidSseCheckpointError,
  SseContentTypeError,
  SseHttpError,
  subscribeToSse,
  type SseCheckpointStore,
  type SseEvent,
  type SseSubscriptionOptions,
} from "../../src/events/sse-subscription.js";
import { FetchTransport, type FetchImplementation } from "../../src/http/fetch-transport.js";
import { validateDiscovery, validateEvent } from "../../src/generated/validators.js";
import type { HubDescriptor, ProtocolEvent } from "../../src/protocol/models.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";

const descriptor = decodeProtocolValue<HubDescriptor>(discovery, validateDiscovery, "discovery");

describe("SSE remediation boundaries", () => {
  it("does not persist an ignored-event checkpoint after its mapper aborts", async () => {
    const controller = new AbortController();
    const reason = new Error("mapper stopped");
    const saved: Array<string | undefined> = [];
    const stream = subscribeToSse({
      transport: new FetchTransport({
        baseUrl: "https://synthetic.invalid",
        fetch: async () =>
          eventStreamResponse("id: ignored_0001\nevent: future.event\ndata: {}\n\n"),
      }),
      path: "/events",
      signal: controller.signal,
      checkpoint: checkpointWith(undefined, saved),
      eventMapper: async () => {
        controller.abort(reason);
        return undefined;
      },
    });
    await expect(stream[Symbol.asyncIterator]().next()).rejects.toBe(reason);
    expect(saved).toEqual([]);
  });
  it("delivers schema-valid metadata larger than 64 KiB through the typed source stream", async () => {
    const text = "🌍".repeat(20_000);
    const metadata = { ...metadataRecord, value: { text } };
    const event = {
      event_id: "event_large_0001",
      event_type: "metadata.changed",
      occurred_at: "2026-08-30T12:40:00.000Z",
      vehicle_id: metadata.vehicle_id,
      resource_id: metadata.metadata_id,
      revision: metadata.revision,
      data: metadata,
    };
    expect(validateEvent(event)).toBe(true);
    const wire = `id: ${event.event_id}\nevent: ${event.event_type}\ndata: ${JSON.stringify(event)}\n\n`;
    expect(new TextEncoder().encode(wire).byteLength).toBeGreaterThan(65_536);
    let fetches = 0;
    const saved: Array<string | undefined> = [];
    const session = sessionWith(async () => {
      fetches += 1;
      return fetches === 1 ? eventStreamResponse(wire) : new Response(null, { status: 204 });
    });
    const iterator = streamProtocolEvents(session, {
      checkpoint: checkpointWith(undefined, saved),
      sleep: async () => undefined,
    })[Symbol.asyncIterator]();

    await expect(iterator.next()).resolves.toEqual({ value: event, done: false });
    expect(saved).toEqual([]);
    await expect(iterator.next()).resolves.toMatchObject({ done: true });
    expect(saved).toEqual([event.event_id]);
    expect(fetches).toBe(2);
  });

  it.each([2_048, 2_049])(
    "admits a loaded checkpoint only through 2048 characters (%i)",
    async (length) => {
      const id = "x".repeat(length);
      const sent: Array<string | null> = [];
      const saved: Array<string | undefined> = [];
      const events = subscribeToSse({
        transport: transportWith(async (_input, init) => {
          sent.push(new Headers(init?.headers).get("Last-Event-ID"));
          return eventStreamResponse("");
        }),
        path: "/events",
        checkpoint: checkpointWith(id, saved),
      });

      if (length === 2_048) {
        await expect(collect(events)).resolves.toEqual([]);
        expect(sent).toEqual([id]);
      } else {
        const error = await collectError(events);
        expect(error).toBeInstanceOf(InvalidSseCheckpointError);
        expect(error).not.toHaveProperty("cause");
        expect(String(error)).not.toContain(id);
        expect(sent).toEqual([]);
      }
      expect(saved).toEqual([]);
    },
  );

  it.each([2_048, 2_049])(
    "bounds ID-only saves and replay headers at 2048 characters (%i)",
    async (length) => {
      const id = "x".repeat(length);
      const saved: Array<string | undefined> = [];
      const sent: Array<string | null> = [];
      let reconnects = 0;
      const events = subscribeToSse({
        transport: transportWith(async (_input, init) => {
          sent.push(new Headers(init?.headers).get("Last-Event-ID"));
          return eventStreamResponse(sent.length === 1 ? `id: ${id}\n\n` : "");
        }),
        path: "/events",
        checkpoint: checkpointWith(undefined, saved),
        reconnect: ({ attempt }) => {
          reconnects += 1;
          return attempt === 1 ? 0 : undefined;
        },
        sleep: async () => undefined,
      });

      if (length === 2_048) {
        await expect(collect(events)).resolves.toEqual([]);
        expect(saved).toEqual([id]);
        expect(sent).toEqual([null, id]);
        expect(reconnects).toBe(2);
      } else {
        await expect(collect(events)).rejects.toBeInstanceOf(InvalidSseCheckpointError);
        expect(saved).toEqual([]);
        expect(sent).toEqual([null]);
        expect(reconnects).toBe(0);
      }
    },
  );

  it.each([2_048, 2_049])(
    "bounds ignored unknown-event checkpoints in the typed stream (%i)",
    async (length) => {
      const id = "x".repeat(length);
      const saved: Array<string | undefined> = [];
      let fetches = 0;
      const events = streamProtocolEvents(
        sessionWith(async () => {
          fetches += 1;
          return fetches === 1
            ? eventStreamResponse(`id: ${id}\nevent: future.changed\ndata: {not JSON}\n\n`)
            : new Response(null, { status: 204 });
        }),
        { checkpoint: checkpointWith(undefined, saved), sleep: async () => undefined },
      );

      if (length === 2_048) {
        await expect(collect(events)).resolves.toEqual([]);
        expect(saved).toEqual([id]);
        expect(fetches).toBe(2);
      } else {
        await expect(collect(events)).rejects.toBeInstanceOf(InvalidSseCheckpointError);
        expect(saved).toEqual([]);
        expect(fetches).toBe(1);
      }
    },
  );

  it.each([2_048, 2_049])(
    "validates a yielded ID at the existing commit boundary (%i)",
    async (length) => {
      const id = "x".repeat(length);
      const saved: Array<string | undefined> = [];
      const iterator = subscribeToSse({
        transport: transportWith(async () => eventStreamResponse(`id: ${id}\ndata: first\n\n`)),
        path: "/events",
        checkpoint: checkpointWith(undefined, saved),
      })[Symbol.asyncIterator]();

      await expect(iterator.next()).resolves.toMatchObject({
        value: { lastEventId: id },
        done: false,
      });
      expect(saved).toEqual([]);
      if (length === 2_048) {
        await expect(iterator.next()).resolves.toMatchObject({ done: true });
        expect(saved).toEqual([id]);
      } else {
        await expect(iterator.next()).rejects.toBeInstanceOf(InvalidSseCheckpointError);
        expect(saved).toEqual([]);
      }
    },
  );

  it("keeps an empty ID reset and omits the replay header after reconnect", async () => {
    const sent: Array<string | null> = [];
    const saved: Array<string | undefined> = [];
    await expect(
      collect(
        subscribeToSse({
          transport: transportWith(async (_input, init) => {
            sent.push(new Headers(init?.headers).get("Last-Event-ID"));
            return eventStreamResponse(sent.length === 1 ? "id:\n\n" : "");
          }),
          path: "/events",
          checkpoint: checkpointWith("x".repeat(2_048), saved),
          reconnect: ({ attempt }) => (attempt === 1 ? 0 : undefined),
          sleep: async () => undefined,
        }),
      ),
    ).resolves.toEqual([]);
    expect(sent).toEqual(["x".repeat(2_048), null]);
    expect(saved).toEqual([undefined]);
  });

  it.each(["wrong MIME", "HTTP error", "terminal classifier", "classifier failure"] as const)(
    "cancels an open response before parser ownership for %s",
    async (caseName) => {
      let cancels = 0;
      const classifierError = new Error("classifier failed");
      const response = openResponse(
        () => {
          cancels += 1;
        },
        {
          status: caseName === "HTTP error" ? 503 : 200,
          contentType: caseName === "wrong MIME" ? "application/json" : "text/event-stream",
        },
      );
      const classifier: SseSubscriptionOptions["responseClassifier"] =
        caseName === "terminal classifier"
          ? () => "terminal"
          : caseName === "classifier failure"
            ? () => {
                throw classifierError;
              }
            : undefined;
      const events = subscribeToSse({
        transport: transportWith(async () => response),
        path: "/events",
        ...(classifier === undefined ? {} : { responseClassifier: classifier }),
      });

      if (caseName === "terminal classifier") {
        await expect(collect(events)).resolves.toEqual([]);
      } else {
        const error = await collectError(events);
        if (caseName === "wrong MIME") expect(error).toBeInstanceOf(SseContentTypeError);
        else if (caseName === "HTTP error") expect(error).toBeInstanceOf(SseHttpError);
        else expect(error).toBe(classifierError);
      }
      expect(cancels).toBe(1);
      expect(response.body?.locked).toBe(false);
    },
  );

  it.each(["fetch", "classifier"] as const)(
    "cancels the unparsed response when abort occurs during %s",
    async (stage) => {
      const controller = new AbortController();
      const reason = new Error("stop subscription");
      let cancels = 0;
      const response = openResponse(() => {
        cancels += 1;
      });
      const events = subscribeToSse({
        transport: transportWith(async () => {
          if (stage === "fetch") controller.abort(reason);
          return response;
        }),
        path: "/events",
        signal: controller.signal,
        ...(stage === "classifier"
          ? {
              responseClassifier: async () => {
                controller.abort(reason);
                return "continue" as const;
              },
            }
          : {}),
      });

      await expect(collect(events)).rejects.toBe(reason);
      expect(cancels).toBe(1);
      expect(response.body?.locked).toBe(false);
    },
  );

  it("preserves MIME rejection when the cancellation hook never settles", async () => {
    let cancels = 0;
    const response = openResponse(
      () => {
        cancels += 1;
        return new Promise<void>(() => {});
      },
      { contentType: "application/json" },
    );

    await expect(
      settlesPromptly(
        collect(
          subscribeToSse({
            transport: transportWith(async () => response),
            path: "/events",
          }),
        ),
      ),
    ).rejects.toBeInstanceOf(SseContentTypeError);
    expect(cancels).toBe(1);
  });

  it("cancels and releases on early consumer return without waiting for a stalled hook or committing", async () => {
    let cancels = 0;
    const saved: Array<string | undefined> = [];
    const response = openResponse(
      () => {
        cancels += 1;
        return new Promise<void>(() => {});
      },
      { wire: "id: event-7\ndata: first\n\n" },
    );
    const iterator = subscribeToSse({
      transport: transportWith(async () => response),
      path: "/events",
      checkpoint: checkpointWith(undefined, saved),
    })[Symbol.asyncIterator]();

    await expect(iterator.next()).resolves.toMatchObject({ value: { data: "first" }, done: false });
    await expect(settlesPromptly(iterator.return?.())).resolves.toMatchObject({ done: true });
    expect(cancels).toBe(1);
    expect(response.body?.locked).toBe(false);
    expect(saved).toEqual([]);
  });

  it("does not cancel a successfully exhausted parser-owned response", async () => {
    let cancels = 0;
    const response = new Response(
      new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode("data: first\n\n"));
          controller.close();
        },
        cancel() {
          cancels += 1;
        },
      }),
      { headers: { "Content-Type": "text/event-stream" } },
    );

    await expect(
      collect(
        subscribeToSse({
          transport: transportWith(async () => response),
          path: "/events",
        }),
      ),
    ).resolves.toMatchObject([{ data: "first" }]);
    expect(cancels).toBe(0);
    expect(response.body?.locked).toBe(false);
  });

  it("records the existing bounded typed reconnect delay sequence over four requests", async () => {
    const responses = [
      eventStreamResponse("retry: 0\n\n"),
      eventStreamResponse(""),
      eventStreamResponse("retry: 50000\n\n"),
      new Response(null, { status: 204 }),
    ];
    let fetches = 0;
    const delays: number[] = [];
    await expect(
      collect(
        streamProtocolEvents(
          sessionWith(async () => {
            fetches += 1;
            const response = responses.shift();
            if (response === undefined) throw new Error("unexpected fetch");
            return response;
          }),
          {
            sleep: async (milliseconds) => {
              delays.push(milliseconds);
            },
          },
        ),
      ),
    ).resolves.toEqual([]);
    expect(fetches).toBe(4);
    expect(delays).toEqual([0, 3_000, 30_000]);
  });
});

function transportWith(fetch: FetchImplementation): FetchTransport {
  return new FetchTransport({ baseUrl: "https://events.example.invalid", fetch });
}

function sessionWith(fetch: FetchImplementation): ClientSession {
  const transport = transportWith(fetch);
  return {
    descriptor,
    protocolVersion: "1.2.0",
    discoveryTransport: transport,
    apiTransport: transport,
    eventTransport: transport,
  };
}

function checkpointWith(
  value: string | undefined,
  saved: Array<string | undefined>,
): SseCheckpointStore {
  return {
    load: () => value,
    save: (next) => {
      saved.push(next);
    },
  };
}

function eventStreamResponse(wire: string): Response {
  return new Response(wire, { headers: { "Content-Type": "text/event-stream" } });
}

function openResponse(
  cancel: () => void | Promise<void>,
  options: { readonly status?: number; readonly contentType?: string; readonly wire?: string } = {},
): Response {
  return new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        if (options.wire !== undefined) controller.enqueue(new TextEncoder().encode(options.wire));
      },
      cancel,
    }),
    {
      status: options.status ?? 200,
      headers: { "Content-Type": options.contentType ?? "text/event-stream" },
    },
  );
}

async function collect<T>(events: AsyncIterable<T>): Promise<T[]> {
  const result: T[] = [];
  for await (const event of events) result.push(event);
  return result;
}

async function collectError(events: AsyncIterable<SseEvent | ProtocolEvent>): Promise<unknown> {
  try {
    await collect(events);
    return undefined;
  } catch (error) {
    return error;
  }
}

async function settlesPromptly<T>(pending: Promise<T> | undefined): Promise<T | undefined> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      pending,
      new Promise<never>((_resolve, reject) => {
        timeout = setTimeout(() => reject(new Error("SSE cleanup did not settle promptly")), 1_000);
      }),
    ]);
  } finally {
    clearTimeout(timeout);
  }
}
