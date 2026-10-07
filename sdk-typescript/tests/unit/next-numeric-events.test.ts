import { describe, expect, it, vi } from "vitest";
import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import envelope from "../../protocol/source/examples/event-envelope.json" with { type: "json" };
import metadata from "../../protocol/source/examples/metadata-record.json" with { type: "json" };
import drives from "../../protocol/source/profiles/hub-http-v1/1.0.0/examples/drives.json" with {
  type: "json",
};
import { createClientSession } from "../../src/client/session.js";
import { streamProtocolEvents } from "../../src/events/protocol-subscription.js";
import { parseSseStream } from "../../src/events/sse-parser.js";
import { subscribeToSse } from "../../src/events/sse-subscription.js";
import { validateMetadataRecord, validateVehiclePage } from "../../src/generated/validators.js";
import { readBoundedJson } from "../../src/http/bounded-json.js";
import { FetchTransport } from "../../src/http/fetch-transport.js";
import { decodeHubJson, validateHubDrives } from "../../src/hub/validate.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";

async function rich(token: string) {
  const raw = JSON.stringify(metadata).replace(/"revision":\d+/u, `"revision":${token}`);
  const value = await readBoundedJson(new Response(raw), undefined, "metadata");
  return decodeProtocolValue<Record<string, unknown>>(value, validateMetadataRecord, "metadata");
}
async function items(text: string) {
  const values = [];
  const body = new Response(text).body;
  if (body === null) throw new Error("missing fixture body");
  for await (const item of parseSseStream(body)) values.push(item);
  return values;
}
describe("next numeric/event corrections", () => {
  it.each(["1.0000000000000001", "9007199254740993", "1e400"])(
    "rejects changed rich revision %s",
    async (token) => {
      await expect(rich(token)).rejects.toMatchObject({ code: "protocol_validation" });
    },
  );
  it.each(["1.0", "10e-1", "9007199254740992", "1e20"])(
    "preserves exact rich revision %s",
    async (token) => {
      expect((await rich(token)).revision).toBe(Number(token));
    },
  );
  it("rejects a nonfinite CurrentHub measurement", () => {
    const raw = JSON.stringify(drives).replace('"distance_km":null', '"distance_km":1e400');
    expect(() => decodeHubJson(raw, validateHubDrives, "drives")).toThrow();
  });
  it.each([1, 2000])("bounds retained invalid-page diagnostics at %s items", (count) => {
    expect(
      validateVehiclePage({ items: Array.from({ length: count }, () => ({})), next_cursor: null }),
    ).toBe(false);
    expect(
      (validateVehiclePage as typeof validateVehiclePage & { errors?: unknown[] }).errors?.length,
    ).toBeLessThanOrEqual(1);
  });
  it("checks cancellation before calling checkpoint load", async () => {
    const signal = AbortSignal.abort(new Error("caller"));
    const load = vi.fn(async () => {
      throw new Error("storage");
    });
    const request = vi.fn(async () => new Response());
    const transport = new FetchTransport({
      baseUrl: "https://synthetic.invalid",
      authorization: () => undefined,
      fetch: request,
    });
    const pending = subscribeToSse({
      transport,
      path: "/",
      signal,
      checkpoint: { load, save: () => {} },
    })
      [Symbol.asyncIterator]()
      .next();
    await expect(pending).rejects.toBe(signal.reason);
    expect(load).not.toHaveBeenCalled();
    expect(request).not.toHaveBeenCalled();
  });
  it("strips one BOM and retains the second field prefix", async () => {
    expect(await items("\uFEFFdata: first\n\n")).toHaveLength(1);
    expect(await items("\uFEFF\uFEFFdata: hidden\n\n")).toHaveLength(0);
  });
});

describe("numeric/event regression boundaries", () => {
  it("honors final duplicate members and escaped integer keys", async () => {
    const raw = JSON.stringify(metadata).replace(
      /"revision":\d+/u,
      '"revision":9007199254740993,"revi\\u0073ion":42.0',
    );
    const value = await readBoundedJson(new Response(raw), undefined, "metadata");
    expect(
      decodeProtocolValue<Record<string, unknown>>(value, validateMetadataRecord, "metadata")
        .revision,
    ).toBe(42);
    await expect(
      readBoundedJson(new Response('{"x":1e400,"x":0.25}'), undefined, "value"),
    ).resolves.toEqual({ x: 0.25 });
  });
  it("preserves finite measurement decimals and arbitrary metadata revision properties", async () => {
    const raw = JSON.stringify(metadata).replace(
      '"value":{',
      '"value":{"revision":42.0000000000000001,',
    );
    const value = await readBoundedJson(new Response(raw), undefined, "metadata");
    expect(raw).toContain('"value":{"revision":42.0000000000000001,');
    expect(decodeProtocolValue(value, validateMetadataRecord, "metadata")).toBe(value);
    expect((value as { value: { revision: number } }).value.revision).toBe(42);
    const page = decodeHubJson(
      JSON.stringify(drives).replace('"distance_km":null', '"distance_km":42.0000000000000001'),
      validateHubDrives,
      "drives",
    );
    expect(page.items[0]?.distanceKm).toBe(42);
  });
  it.each(["9007199254740992", "9007199254740994", "101.0", "10100e-2"])(
    "preserves exact CurrentHub ID %s",
    (token) => {
      const raw = JSON.stringify(drives).replace('"id":101', `"id":${token}`);
      expect(decodeHubJson(raw, validateHubDrives, "drives").items[0]?.id).toBe(Number(token));
    },
  );
  it("rejects nonfinite directly constructed values but keeps exact integer domain", () => {
    expect(() =>
      decodeProtocolValue({ ...metadata, revision: Infinity }, validateMetadataRecord, "metadata"),
    ).toThrow();
    expect(
      decodeProtocolValue({ ...metadata, revision: 1e20 }, validateMetadataRecord, "metadata"),
    ).toMatchObject({ revision: 1e20 });
  });
  it("interrupts an idle body from a custom signal-uncoupled transport", async () => {
    let readStarted!: () => void;
    const started = new Promise<void>((resolve) => {
      readStarted = resolve;
    });
    const cancelled = vi.fn(() => new Promise<void>(() => {}));
    const body = new ReadableStream<Uint8Array>(
      {
        pull() {
          readStarted();
        },
        cancel: cancelled,
      },
      { highWaterMark: 0 },
    );
    const signal = new AbortController();
    const transport = new FetchTransport({
      baseUrl: "https://synthetic.invalid",
      authorization: () => undefined,
      fetch: async () => new Response(body, { headers: { "Content-Type": "text/event-stream" } }),
    });
    const pending = subscribeToSse({ transport, path: "/", signal: signal.signal })
      [Symbol.asyncIterator]()
      .next();
    await started;
    const reason = new Error("stop idle supplied body");
    signal.abort(reason);
    await expect(pending).rejects.toBe(reason);
    expect(cancelled).toHaveBeenCalledTimes(1);
    expect(body.locked).toBe(false);
  });
  it("known event numeric disagreement rejects without a replay commit", async () => {
    const saved = vi.fn();
    let ordinal = 0;
    const wire = JSON.stringify(envelope).replace(
      /"revision":\d+/gu,
      () => `"revision":${++ordinal === 1 ? "9007199254740992" : "9007199254740993"}`,
    );
    const session = await createClientSession({
      baseUrl: "https://synthetic.invalid",
      authorization: () => undefined,
      fetch: async (input) =>
        String(input).includes(".well-known")
          ? Response.json(discovery, { headers: { ETag: '"discovery"' } })
          : new Response(
              "id: " +
                envelope.event_id +
                "\nevent: " +
                envelope.event_type +
                "\ndata: " +
                wire +
                "\n\n",
              { headers: { "Content-Type": "text/event-stream" } },
            ),
    });
    const iterator = streamProtocolEvents(session, {
      checkpoint: { load: () => undefined, save: saved },
    })[Symbol.asyncIterator]();
    await expect(iterator.next()).rejects.toMatchObject({ code: "protocol_validation" });
    expect(saved).not.toHaveBeenCalled();
  });
});

it("preserves a long data line under single-byte transport fragmentation", async () => {
  const wire = new TextEncoder().encode(`data: ${"x".repeat(65_536)}\n\n`);
  let offset = 0;
  const stream = new ReadableStream<Uint8Array>(
    {
      pull(c) {
        if (offset === wire.length) c.close();
        else c.enqueue(wire.subarray(offset, ++offset));
      },
    },
    { highWaterMark: 0 },
  );
  const output = [];
  for await (const item of parseSseStream(stream)) output.push(item);
  expect(output).toEqual([
    { type: "event", event: "message", lastEventId: "", data: "x".repeat(65_536) },
  ]);
});

it("cancels instead of publishing another buffered event after abort", async () => {
  const signal = new AbortController();
  const body = new Response("data: one\n\ndata: two\n\n").body;
  if (body === null) throw new Error("missing fixture body");
  const iterator = parseSseStream(body, { signal: signal.signal })[Symbol.asyncIterator]();
  expect((await iterator.next()).value).toMatchObject({ data: "one" });
  const reason = new Error("stop before buffered publish");
  signal.abort(reason);
  await expect(iterator.next()).rejects.toBe(reason);
  expect(body.locked).toBe(false);
});

it.each(["9223372036854774784", "-9223372036854775808"])(
  "preserves signed-i64 representable ID %s",
  (token) => {
    const raw = JSON.stringify(drives).replace('"id":101', `"id":${token}`);
    expect(decodeHubJson(raw, validateHubDrives, "drives").items[0]?.id).toBe(Number(token));
  },
);
it("rejects signed-i64 overflow hidden by a rounded schema maximum", () => {
  const raw = JSON.stringify(drives).replace('"id":101', '"id":9223372036854775808');
  expect(() => decodeHubJson(raw, validateHubDrives, "drives")).toThrow();
});
