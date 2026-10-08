import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import eventEnvelope from "../../protocol/source/examples/event-envelope.json" with {
  type: "json",
};
import { describe, expect, it } from "vitest";
import { TeslatlasClient } from "../../src/client/client.js";
import type { ClientSession } from "../../src/client/types.js";
import {
  InvalidSseCheckpointError,
  type SseCheckpointStore,
  type SseSleep,
} from "../../src/events/sse-subscription.js";
import { FetchTransport, type FetchImplementation } from "../../src/http/fetch-transport.js";
import { validateDiscovery } from "../../src/generated/validators.js";
import type { HubDescriptor, ProtocolEvent } from "../../src/protocol/models.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";
import {
  MissingCapabilityError,
  ProtocolHttpError,
  ProtocolValidationError,
  ReplayGapError,
} from "../../src/core/errors.js";
import {
  InvalidStreamEventsOptionsError,
  UnsupportedStreamEventTypeError,
} from "../../src/events/protocol-subscription.js";

const descriptor = decodeProtocolValue<HubDescriptor>(discovery, validateDiscovery, "discovery");

interface StreamEventsOptions {
  readonly vehicleId?: string;
  readonly eventTypes?: readonly ProtocolEvent["event_type"][];
  readonly checkpoint?: SseCheckpointStore;
  readonly signal?: AbortSignal;
  readonly sleep?: SseSleep;
}

interface EventClient {
  streamEvents(options: StreamEventsOptions): AsyncIterable<ProtocolEvent>;
}

describe("typed protocol events", () => {
  it("sends the closed event request and yields only a validated protocol event", async () => {
    const observed: Array<{
      path: string;
      headers: Headers;
      redirect: RequestRedirect | undefined;
    }> = [];
    const client = createClient(async (input, init) => {
      const url = new URL(String(input));
      observed.push({
        path: `${url.pathname}${url.search}`,
        headers: new Headers(init?.headers),
        redirect: init?.redirect,
      });
      return eventStream(eventEnvelope);
    });

    const events = await take(
      streamEvents(client, {
        vehicleId: "vehicle_demo_alpha",
        eventTypes: ["vehicle.current.changed"],
      }),
      1,
    );

    expect(events).toEqual([eventEnvelope]);
    expect(observed).toHaveLength(1);
    expect(observed[0]).toMatchObject({
      path: "/v1/events?vehicle_id=vehicle_demo_alpha&event_type=vehicle.current.changed",
      redirect: "error",
    });
    expect(observed[0]?.headers.get("accept")).toBe("text/event-stream");
    expect(observed[0]?.headers.get("authorization")).toBe("Bearer caller-owned");
    expect(observed[0]?.headers.get("teslatlas-protocol-version")).toBe("1.2.0");
  });

  it("encodes event filters once with form explode false", async () => {
    const observed: string[] = [];
    const client = createClient(async (input) => {
      const url = new URL(String(input));
      observed.push(`${url.pathname}${url.search}`);
      return new Response(null, { status: 204 });
    });

    await expect(
      collect(
        streamEvents(client, {
          eventTypes: ["vehicle.current.changed", "drive.started"],
        }),
      ),
    ).resolves.toEqual([]);

    expect(observed).toEqual(["/v1/events?event_type=vehicle.current.changed%2Cdrive.started"]);
  });

  it("checkpoints an ignored unknown event without parsing its data", async () => {
    const saved: Array<string | undefined> = [];
    const responses = [
      eventStreamWire(
        [
          "id: event_unknown_0001\nevent: future.changed\ndata: {not JSON}",
          knownEventWire(eventEnvelope),
        ].join("\n\n"),
      ),
      new Response(null, { status: 204 }),
    ];
    const client = createClient(async () => {
      const response = responses.shift();
      if (response === undefined) throw new Error("unexpected Fetch");
      return response;
    });
    const iterator = streamEvents(client, {
      checkpoint: {
        load: () => undefined,
        save: (value) => {
          saved.push(value);
        },
      },
      sleep: async () => undefined,
    })[Symbol.asyncIterator]();

    await expect(iterator.next()).resolves.toMatchObject({ value: eventEnvelope, done: false });
    expect(saved).toEqual(["event_unknown_0001"]);
    await expect(iterator.next()).resolves.toMatchObject({ done: true });
    expect(saved).toEqual(["event_unknown_0001", "event_demo_0042"]);
  });

  it("treats prototype-named SSE events as unknown and checkpoints them", async () => {
    const saved: Array<string | undefined> = [];
    const responses = [
      eventStreamWire(
        [
          "id: event_unknown_0002\nevent: constructor\ndata: {not JSON}",
          knownEventWire(eventEnvelope),
        ].join("\n\n"),
      ),
      new Response(null, { status: 204 }),
    ];
    const client = createClient(async () => {
      const response = responses.shift();
      if (response === undefined) throw new Error("unexpected Fetch");
      return response;
    });
    const iterator = streamEvents(client, {
      checkpoint: {
        load: () => undefined,
        save: (value) => {
          saved.push(value);
        },
      },
      sleep: async () => undefined,
    })[Symbol.asyncIterator]();

    await expect(iterator.next()).resolves.toMatchObject({ value: eventEnvelope, done: false });
    expect(saved).toEqual(["event_unknown_0002"]);
    await expect(iterator.next()).resolves.toMatchObject({ done: true });
    expect(saved).toEqual(["event_unknown_0002", "event_demo_0042"]);
  });

  it.each([
    ["malformed JSON", knownEventWireText("{not JSON}")],
    ["schema mismatch", knownEventWireText("{}")],
    [
      "SSE ID mismatch",
      knownEventWire({ ...eventEnvelope, event_id: "event_demo_9999" }, "event_demo_0042"),
    ],
    ["SSE type mismatch", knownEventWire({ ...eventEnvelope, event_type: "drive.started" })],
    ["semantic revision mismatch", knownEventWire({ ...eventEnvelope, revision: 41 })],
    [
      "semantic vehicle mismatch",
      knownEventWire({ ...eventEnvelope, vehicle_id: "vehicle_demo_other" }),
    ],
    [
      "semantic resource mismatch",
      knownEventWire({ ...eventEnvelope, resource_id: "vehicle_demo_other" }),
    ],
  ])("stops for a known event with %s", async (_label, wire) => {
    const client = createClient(async () => eventStreamWire(wire));

    await expect(collect(streamEvents(client))).rejects.toBeInstanceOf(ProtocolValidationError);
  });

  it("saves an empty ID immediately but commits a yielded ID only on next", async () => {
    const saved: Array<string | undefined> = [];
    const checkpoint: SseCheckpointStore = {
      load: () => undefined,
      save: (value) => {
        saved.push(value);
      },
    };
    const responses = [
      eventStreamWire(`id:\n\n${knownEventWire(eventEnvelope)}`),
      new Response(null, { status: 204 }),
    ];
    const client = createClient(async () => {
      const response = responses.shift();
      if (response === undefined) throw new Error("unexpected Fetch");
      return response;
    });
    const iterator = streamEvents(client, { checkpoint, sleep: async () => undefined })[
      Symbol.asyncIterator
    ]();

    await expect(iterator.next()).resolves.toMatchObject({ value: eventEnvelope, done: false });
    expect(saved).toEqual([undefined]);
    await expect(iterator.next()).resolves.toMatchObject({ done: true });
    expect(saved).toEqual([undefined, "event_demo_0042"]);
  });

  it.each([null, 42, {}, []])("rejects a malformed checkpoint load value %j", async (value) => {
    let fetchCalls = 0;
    const client = createClient(async () => {
      fetchCalls += 1;
      return new Response(null, { status: 204 });
    });
    const checkpoint: SseCheckpointStore = {
      load: () => value as unknown as string,
      save: () => undefined,
    };

    await expect(collect(streamEvents(client, { checkpoint }))).rejects.toBeInstanceOf(
      InvalidSseCheckpointError,
    );
    expect(fetchCalls).toBe(0);
  });

  it("maps terminal problem responses without retrying", async () => {
    const cases = [
      [400, "event_id_invalid", ProtocolHttpError],
      [410, "event_replay_expired", ReplayGapError],
      [404, "not_found", ProtocolHttpError],
    ] as const;

    for (const [status, code, expected] of cases) {
      let calls = 0;
      const client = createClient(async () => {
        calls += 1;
        return problemResponse(status, code);
      });

      const error = await collectError(streamEvents(client));
      expect(error).toBeInstanceOf(expected);
      expect(error).toMatchObject({ status, code });
      expect(calls).toBe(1);
    }
  });

  it("gates event capability and profile-specific names before Fetch", async () => {
    let calls = 0;
    const noEvents: HubDescriptor = {
      ...descriptor,
      capabilities: descriptor.capabilities.filter(({ id }) => id !== "events.sse"),
    };
    const noCapabilityClient = createClient(async () => {
      calls += 1;
      return new Response(null, { status: 204 });
    }, noEvents);
    expect(() => streamEvents(noCapabilityClient)).toThrow(MissingCapabilityError);

    const oldProfileClient = createClient(
      async () => {
        calls += 1;
        return new Response(null, { status: 204 });
      },
      descriptor,
      "1.0.0",
    );
    expect(() => streamEvents(oldProfileClient, { eventTypes: ["command.changed"] })).toThrow(
      UnsupportedStreamEventTypeError,
    );
    const middleProfileClient = createClient(
      async () => new Response(null, { status: 204 }),
      descriptor,
      "1.1.0",
    );
    expect(() => streamEvents(middleProfileClient, { eventTypes: ["metadata.changed"] })).toThrow(
      UnsupportedStreamEventTypeError,
    );
    const currentProfileClient = createClient(
      async () => new Response(null, { status: 204 }),
      descriptor,
      "1.2.0",
    );
    expect(() =>
      streamEvents(currentProfileClient, { eventTypes: ["metadata.changed"] }),
    ).not.toThrow();
    expect(() =>
      streamEvents(oldProfileClient, {
        eventTypes: ["vehicle.current.changed", "vehicle.current.changed"],
      }),
    ).toThrow(InvalidStreamEventsOptionsError);
    expect(() =>
      streamEvents(oldProfileClient, {
        eventTypes: Array.from({ length: 33 }, () => "vehicle.current.changed"),
      }),
    ).toThrow(InvalidStreamEventsOptionsError);
    expect(() =>
      streamEvents(oldProfileClient, {
        eventTypes: ["constructor" as unknown as ProtocolEvent["event_type"]],
      }),
    ).toThrow(InvalidStreamEventsOptionsError);
    expect(calls).toBe(0);
  });

  it("keeps replay caller-scoped across distinct authorization providers and stores", async () => {
    const savedByA: Array<string | undefined> = [];
    const aResponses = [eventStream(eventEnvelope), new Response(null, { status: 204 })];
    const clientA = createClient(
      async (_input, init) => {
        expect(new Headers(init?.headers).get("authorization")).toBe("Bearer principal-a");
        const response = aResponses.shift();
        if (response === undefined) throw new Error("unexpected principal A Fetch");
        return response;
      },
      descriptor,
      "1.2.0",
      () => "Bearer principal-a",
    );
    const checkpointA: SseCheckpointStore = {
      load: () => undefined,
      save: (value) => {
        savedByA.push(value);
      },
    };
    const aIterator = streamEvents(clientA, {
      checkpoint: checkpointA,
      sleep: async () => undefined,
    })[Symbol.asyncIterator]();
    await expect(aIterator.next()).resolves.toMatchObject({ value: eventEnvelope, done: false });
    await expect(aIterator.next()).resolves.toMatchObject({ done: true });
    expect(savedByA).toEqual(["event_demo_0042"]);

    let bEventCalls = 0;
    const clientB = createClient(
      async (input, init) => {
        const url = new URL(String(input));
        const headers = new Headers(init?.headers);
        expect(headers.get("authorization")).toBe("Bearer principal-b");
        if (url.pathname.endsWith("/current")) return problemResponse(404, "not_found");
        if (url.pathname !== "/v1/events")
          throw new Error(`unexpected principal B path ${url.pathname}`);
        bEventCalls += 1;
        if (headers.get("last-event-id") === "event_demo_0042") {
          return problemResponse(400, "event_id_invalid");
        }
        return bEventCalls === 1 ? eventStreamWire("") : new Response(null, { status: 204 });
      },
      descriptor,
      "1.2.0",
      () => "Bearer principal-b",
    );

    await expect(clientB.getVehicleCurrentState("vehicle_demo_alpha")).rejects.toMatchObject({
      code: "not_found",
      status: 404,
    });
    await expect(
      collect(
        streamEvents(clientB, {
          checkpoint: { load: () => undefined, save: () => undefined },
          sleep: async () => undefined,
        }),
      ),
    ).resolves.toEqual([]);

    const replayError = await collectError(
      streamEvents(clientB, {
        checkpoint: { load: () => savedByA[0], save: () => undefined },
      }),
    );
    expect(replayError).toBeInstanceOf(ProtocolHttpError);
    expect(replayError).toMatchObject({ code: "event_id_invalid", status: 400 });
    expect(bEventCalls).toBe(3);
  });
});

function streamEvents(
  client: TeslatlasClient,
  options: StreamEventsOptions = {},
): AsyncIterable<ProtocolEvent> {
  return (client as unknown as EventClient).streamEvents(options);
}

function createClient(
  fetch: FetchImplementation,
  sessionDescriptor: HubDescriptor = descriptor,
  protocolVersion: "1.0.0" | "1.1.0" | "1.2.0" = "1.2.0",
  authorization: () => string = () => "Bearer caller-owned",
): TeslatlasClient {
  const eventTransport = new FetchTransport({
    baseUrl: "https://events.example.invalid",
    authorization,
    fetch,
  });
  const session: ClientSession = {
    descriptor: sessionDescriptor,
    protocolVersion,
    discoveryTransport: new FetchTransport({ baseUrl: "https://hub.example.invalid", fetch }),
    apiTransport: new FetchTransport({
      baseUrl: "https://api.example.invalid",
      authorization,
      fetch,
    }),
    eventTransport,
  };
  return new TeslatlasClient(session);
}

function eventStream(event: unknown): Response {
  return eventStreamWire(knownEventWire(event));
}

function eventStreamWire(wire: string): Response {
  return new Response(wire, { headers: { "Content-Type": "text/event-stream; charset=utf-8" } });
}

function knownEventWire(value: unknown, id = eventEnvelope.event_id): string {
  return `id: ${id}\nevent: ${eventEnvelope.event_type}\ndata: ${JSON.stringify(value)}\n\n`;
}

function knownEventWireText(value: string): string {
  return `id: ${eventEnvelope.event_id}\nevent: ${eventEnvelope.event_type}\ndata: ${value}\n\n`;
}

function problemResponse(status: number, code: string): Response {
  return Response.json(
    {
      type: `urn:teslatlas:problem:${code.replaceAll("_", "-")}`,
      title: "Protocol problem",
      status,
      code,
      request_id: "request_demo_0001",
      instance: "/requests/request_demo_0001",
      retryable: false,
    },
    { status, headers: { "Content-Type": "application/problem+json" } },
  );
}

async function take<T>(events: AsyncIterable<T>, count: number): Promise<T[]> {
  const result: T[] = [];
  for await (const event of events) {
    result.push(event);
    if (result.length === count) break;
  }
  return result;
}

async function collect<T>(events: AsyncIterable<T>): Promise<T[]> {
  const result: T[] = [];
  for await (const event of events) result.push(event);
  return result;
}

async function collectError(events: AsyncIterable<unknown>): Promise<unknown> {
  try {
    await collect(events);
    return undefined;
  } catch (error) {
    return error;
  }
}
