import { describe, expect, it } from "vitest";
import { FetchTransport, type FetchImplementation } from "../../src/http/fetch-transport.js";
import {
  InvalidSseCheckpointError,
  SseContentTypeError,
  SseHttpError,
  subscribeToSse,
  type SseCheckpointStore,
  type SseEvent,
} from "../../src/events/sse-subscription.js";

describe("SSE subscription", () => {
  it("sends Accept and a non-empty caller-owned checkpoint", async () => {
    let seenHeaders = new Headers();
    let loads = 0;
    const checkpoint: SseCheckpointStore = {
      load: () => {
        loads += 1;
        return "event-6";
      },
      save: () => undefined,
    };
    const transport = transportWith(async (_input, init) => {
      seenHeaders = new Headers(init?.headers);
      return eventStreamResponse("data: resumed\n\n");
    });

    const events = await take(subscribeToSse({ transport, path: "/events", checkpoint }), 1);

    expect(events[0]).toMatchObject({ data: "resumed", lastEventId: "event-6" });
    expect(seenHeaders.get("accept")).toBe("text/event-stream");
    expect(seenHeaders.get("last-event-id")).toBe("event-6");
    expect(loads).toBe(1);
  });

  it("commits an event ID only after the consumer requests another event", async () => {
    const saved: Array<string | undefined> = [];
    const checkpoint: SseCheckpointStore = {
      load: () => undefined,
      save: (value) => {
        saved.push(value);
      },
    };
    const transport = transportWith(async () =>
      eventStreamResponse("id: event-7\ndata: first\n\n"),
    );
    const iterator = subscribeToSse({ transport, path: "/events", checkpoint })[
      Symbol.asyncIterator
    ]();

    const first = await iterator.next();
    expect(first.value).toMatchObject({ data: "first", lastEventId: "event-7" });
    expect(saved).toEqual([]);

    const end = await iterator.next();
    expect(end.done).toBe(true);
    expect(saved).toEqual(["event-7"]);
  });

  it("does not commit a yielded event when the consumer returns early", async () => {
    const saved: Array<string | undefined> = [];
    const checkpoint: SseCheckpointStore = {
      load: () => undefined,
      save: (value) => {
        saved.push(value);
      },
    };
    const transport = transportWith(async () =>
      eventStreamResponse("id: event-7\ndata: first\n\n"),
    );
    const iterator = subscribeToSse({ transport, path: "/events", checkpoint })[
      Symbol.asyncIterator
    ]();

    await iterator.next();
    await iterator.return?.();

    expect(saved).toEqual([]);
  });

  it("commits an ID-only block because it has no consumer event", async () => {
    const saved: Array<string | undefined> = [];
    const checkpoint: SseCheckpointStore = {
      load: () => "event-6",
      save: (value) => {
        saved.push(value);
      },
    };
    const transport = transportWith(async () => eventStreamResponse("id:\n\n"));

    const events = await collect(subscribeToSse({ transport, path: "/events", checkpoint }));

    expect(events).toEqual([]);
    expect(saved).toEqual([undefined]);
  });

  it("reconnects with the last committed event ID", async () => {
    const headers: Array<string | null> = [];
    const responses = [
      eventStreamResponse("id: event-7\ndata: first\n\n"),
      eventStreamResponse("data: second\n\n"),
    ];
    const transport = transportWith(async (_input, init) => {
      headers.push(new Headers(init?.headers).get("last-event-id"));
      const response = responses.shift();
      if (response === undefined) {
        throw new Error("unexpected fetch");
      }
      return response;
    });
    const attempts: number[] = [];

    const events = await take(
      subscribeToSse({
        transport,
        path: "/events",
        reconnect: ({ attempt }) => {
          attempts.push(attempt);
          return attempt === 1 ? 0 : undefined;
        },
        sleep: async () => undefined,
      }),
      2,
    );

    expect(events.map((event) => event.data)).toEqual(["first", "second"]);
    expect(headers).toEqual([null, "event-7"]);
    expect(attempts).toEqual([1]);
  });

  it("uses a server retry hint once and clamps it to the configured maximum", async () => {
    const responses = [
      eventStreamResponse("retry: 5000\n\n"),
      eventStreamResponse("data: connected\n\n"),
    ];
    const transport = transportWith(async () => {
      const response = responses.shift();
      if (response === undefined) {
        throw new Error("unexpected fetch");
      }
      return response;
    });
    const sleeps: number[] = [];

    const events = await take(
      subscribeToSse({
        transport,
        path: "/events",
        reconnect: () => 25,
        maximumServerRetryMilliseconds: 100,
        sleep: async (milliseconds) => {
          sleeps.push(milliseconds);
        },
      }),
      1,
    );

    expect(events[0]?.data).toBe("connected");
    expect(sleeps).toEqual([100]);
  });

  it("accepts event-stream media type case and parameters", async () => {
    const transport = transportWith(async () =>
      eventStreamResponse("data: ok\n\n", { contentType: "Text/Event-Stream; Charset=UTF-8" }),
    );

    const events = await take(subscribeToSse({ transport, path: "/events" }), 1);

    expect(events[0]?.data).toBe("ok");
  });

  it.each([undefined, "application/json", "text/event-streaming"])(
    "rejects invalid event-stream content type %s",
    async (contentType) => {
      const transport = transportWith(async () =>
        eventStreamResponse("data: no\n\n", { contentType }),
      );

      await expect(collect(subscribeToSse({ transport, path: "/events" }))).rejects.toThrow(
        SseContentTypeError,
      );
    },
  );

  it("returns a typed HTTP error without parsing its body or retrying by default", async () => {
    let calls = 0;
    const transport = transportWith(async () => {
      calls += 1;
      return new Response("data: must-not-dispatch\n\n", {
        status: 503,
        headers: { "Content-Type": "text/event-stream" },
      });
    });

    const error = await collectError(subscribeToSse({ transport, path: "/events" }));

    expect(error).toMatchObject({
      name: "SseHttpError",
      status: 503,
      message: "Teslatlas event stream request failed",
    });
    expect(error).toBeInstanceOf(SseHttpError);
    expect(calls).toBe(1);
  });

  it("stops cleanly after EOF when reconnect policy refuses", async () => {
    let calls = 0;
    const transport = transportWith(async () => {
      calls += 1;
      return eventStreamResponse("");
    });

    const events = await collect(
      subscribeToSse({ transport, path: "/events", reconnect: () => undefined }),
    );

    expect(events).toEqual([]);
    expect(calls).toBe(1);
  });

  it("propagates abort during reconnect sleep and performs no second fetch", async () => {
    const controller = new AbortController();
    const reason = new DOMException("Stopped", "AbortError");
    let calls = 0;
    const transport = transportWith(async () => {
      calls += 1;
      return eventStreamResponse("");
    });

    const subscription = collect(
      subscribeToSse({
        transport,
        path: "/events",
        signal: controller.signal,
        reconnect: () => 20,
        sleep: async () => {
          controller.abort(reason);
          throw reason;
        },
      }),
    );

    await expect(subscription).rejects.toBe(reason);
    expect(calls).toBe(1);
  });

  it("rejects an unsafe stored Last-Event-ID before fetching", async () => {
    let calls = 0;
    const checkpoint: SseCheckpointStore = {
      load: () => "event-6\r\nAuthorization: secret",
      save: () => undefined,
    };
    const transport = transportWith(async () => {
      calls += 1;
      return eventStreamResponse("");
    });

    await expect(
      collect(subscribeToSse({ transport, path: "/events", checkpoint })),
    ).rejects.toThrow(InvalidSseCheckpointError);
    expect(calls).toBe(0);
  });

  it("does not expose a credential-bearing checkpoint-store failure", async () => {
    const checkpoint: SseCheckpointStore = {
      load: () => {
        throw new Error("checkpoint failed with Bearer secret-value");
      },
      save: () => undefined,
    };
    const transport = transportWith(async () => eventStreamResponse(""));

    const error = await collectError(subscribeToSse({ transport, path: "/events", checkpoint }));

    expect(error).toBeInstanceOf(InvalidSseCheckpointError);
    expect(error).not.toHaveProperty("cause");
    expect(String(error)).not.toContain("secret-value");
  });
});

function transportWith(fetch: FetchImplementation): FetchTransport {
  return new FetchTransport({ baseUrl: "https://hub.example", fetch });
}

function eventStreamResponse(
  body: string,
  options: { readonly contentType?: string | undefined } = {},
): Response {
  const headers = new Headers();
  if (options.contentType !== undefined) {
    headers.set("Content-Type", options.contentType);
  } else if (!("contentType" in options)) {
    headers.set("Content-Type", "text/event-stream");
  }
  return new Response(body, { status: 200, headers });
}

async function collect(events: AsyncIterable<SseEvent>): Promise<SseEvent[]> {
  const result: SseEvent[] = [];
  for await (const event of events) {
    result.push(event);
  }
  return result;
}

async function take(events: AsyncIterable<SseEvent>, count: number): Promise<SseEvent[]> {
  const result: SseEvent[] = [];
  for await (const event of events) {
    result.push(event);
    if (result.length === count) {
      break;
    }
  }
  return result;
}

async function collectError(events: AsyncIterable<SseEvent>): Promise<unknown> {
  try {
    await collect(events);
    return undefined;
  } catch (error) {
    return error;
  }
}
