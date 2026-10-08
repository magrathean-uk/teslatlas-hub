import { describe, expect, it } from "vitest";
import { parseSseStream, type SseStreamItem } from "../../src/events/sse-parser.js";

describe("incremental SSE parser", () => {
  it("decodes split UTF-8, BOM, CRLF, event, data, and ID fields", async () => {
    const encoded = new TextEncoder().encode(
      "\uFEFFevent: telemetry\r\ndata: hello\r\ndata: w🌍rld\r\nid: event-7\r\n\r\n",
    );
    const chunks = [...encoded].map((byte) => new Uint8Array([byte]));

    await expect(collect(parseSseStream(streamFrom(chunks)))).resolves.toEqual([
      {
        type: "event",
        event: "telemetry",
        data: "hello\nw🌍rld",
        lastEventId: "event-7",
      },
    ]);
  });

  it("handles CR, LF, split CRLF, comments, and unknown fields", async () => {
    const chunks = [
      encode(": comment\rdata: first\r"),
      encode("\nunknown: ignored\ndata: second\n\n"),
    ];

    await expect(collect(parseSseStream(streamFrom(chunks)))).resolves.toEqual([
      { type: "event", event: "message", data: "first\nsecond", lastEventId: "" },
    ]);
  });

  it("persists IDs across events and emits an ID-only checkpoint", async () => {
    const body = [
      "id: event-1\ndata: first\n\n",
      "data: second\n\n",
      "id:\n\n",
      "data: third\n\n",
    ].join("");

    await expect(collect(parseSseStream(streamFrom([encode(body)])))).resolves.toEqual([
      { type: "event", event: "message", data: "first", lastEventId: "event-1" },
      { type: "event", event: "message", data: "second", lastEventId: "event-1" },
      { type: "checkpoint", lastEventId: "" },
      { type: "event", event: "message", data: "third", lastEventId: "" },
    ]);
  });

  it("ignores an ID containing NUL", async () => {
    const body = "id: safe\ndata: first\n\nid: bad\0id\ndata: second\n\n";

    await expect(collect(parseSseStream(streamFrom([encode(body)])))).resolves.toEqual([
      { type: "event", event: "message", data: "first", lastEventId: "safe" },
      { type: "event", event: "message", data: "second", lastEventId: "safe" },
    ]);
  });

  it("emits only nonnegative safe integer retry hints", async () => {
    const body = [
      "retry: 0\n",
      "retry: 1500\n",
      "retry: -1\n",
      "retry: 1.5\n",
      "retry: 9007199254740992\n",
      "\n",
    ].join("");

    await expect(collect(parseSseStream(streamFrom([encode(body)])))).resolves.toEqual([
      { type: "retry", milliseconds: 0 },
      { type: "retry", milliseconds: 1500 },
    ]);
  });

  it("does not dispatch an unterminated event at EOF", async () => {
    const body = "event: telemetry\ndata: incomplete";

    await expect(collect(parseSseStream(streamFrom([encode(body)])))).resolves.toEqual([]);
  });

  it("starts event IDs from the supplied committed checkpoint", async () => {
    const body = "data: resumed\n\n";

    await expect(
      collect(parseSseStream(streamFrom([encode(body)]), { initialLastEventId: "event-6" })),
    ).resolves.toEqual([
      { type: "event", event: "message", data: "resumed", lastEventId: "event-6" },
    ]);
  });
  it("rejects an unterminated UTF-8 line beyond the event budget plus framing and cancels upstream", async () => {
    let cancelled = false;
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encode("🌍".repeat(Math.floor((8 * 1_024 * 1_024 + 6) / 4) + 1)));
      },
      cancel() {
        cancelled = true;
      },
    });
    await expect(collect(parseSseStream(stream))).rejects.toMatchObject({
      code: "protocol_validation",
      validator: "Sse.lineSize",
    });
    expect(cancelled).toBe(true);
  });

  it("accepts a single data line at the exact aggregate byte bound", async () => {
    const data = "a".repeat(8 * 1_024 * 1_024 - 1);
    await expect(
      collect(parseSseStream(streamFrom([encode(`data: ${data}\n\n`)]))),
    ).resolves.toMatchObject([{ type: "event", data }]);
  });

  it("rejects a single data line beyond the aggregate byte bound", async () => {
    await expect(
      collect(parseSseStream(streamFrom([encode(`data: ${"a".repeat(8 * 1_024 * 1_024)}\n\n`)]))),
    ).rejects.toMatchObject({ validator: "Sse.eventSize" });
  });

  it("bounds many data lines before an event terminator", async () => {
    let cancelled = false;
    const line = encode(`data: ${"a".repeat(65_000)}\n`);
    const stream = new ReadableStream<Uint8Array>({
      pull(controller) {
        controller.enqueue(line);
      },
      cancel() {
        cancelled = true;
      },
    });
    await expect(collect(parseSseStream(stream))).rejects.toMatchObject({
      code: "protocol_validation",
      validator: "Sse.eventSize",
    });
    expect(cancelled).toBe(true);
  });
});

function encode(value: string): Uint8Array {
  return new TextEncoder().encode(value);
}

function streamFrom(chunks: readonly Uint8Array[]): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) {
        controller.enqueue(chunk);
      }
      controller.close();
    },
  });
}

async function collect(items: AsyncIterable<SseStreamItem>): Promise<SseStreamItem[]> {
  const result: SseStreamItem[] = [];
  for await (const item of items) {
    result.push(item);
  }
  return result;
}
