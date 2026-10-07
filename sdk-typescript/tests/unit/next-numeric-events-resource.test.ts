import { writeFile } from "node:fs/promises";
import { expect, it } from "vitest";
import { parseSseStream } from "../../src/events/sse-parser.js";
it.each(["line", "empty-lines"])("records bounded parser allocation: %s", async (kind) => {
  const size = 1_048_576;
  const wire = kind === "line" ? `data: ${"x".repeat(size)}\n\n` : `${"data\n".repeat(size)}\n`;
  const bytes = new TextEncoder().encode(wire);
  let offset = 0;
  const stream = new ReadableStream<Uint8Array>({
    pull(c) {
      if (offset === bytes.length) c.close();
      else {
        c.enqueue(bytes.subarray(offset, offset + 4096));
        offset += Math.min(4096, bytes.length - offset);
      }
    },
  });
  const before = process.memoryUsage();
  const cpu = process.cpuUsage();
  const start = performance.now();
  let length = 0;
  let count = 0;
  for await (const item of parseSseStream(stream)) {
    if (item.type === "event") {
      length = item.data.length;
      count++;
    }
  }
  const after = process.memoryUsage();
  const used = process.cpuUsage(cpu);
  expect(count).toBe(1);
  expect(length).toBe(kind === "line" ? size : size - 1);
  await writeFile(
    "/Users/bolyki/dev/documents/reviews/teslatlas-bayesian-2026-10-07/next-round/teslatlas-sdk-typescript/numeric-events-resource-" +
      (process.env.NUMERIC_EVENTS_PHASE ?? "after") +
      "-" +
      kind +
      ".json",
    JSON.stringify(
      {
        kind,
        size,
        wireBytes: bytes.length,
        chunkBytes: 4096,
        eventCount: count,
        dataLength: length,
        wallMilliseconds: performance.now() - start,
        cpuMicroseconds: used.user + used.system,
        heapIncrease: after.heapUsed - before.heapUsed,
        rssIncrease: after.rss - before.rss,
        node: process.version,
        limits:
          "Single shared Vitest worker sample; input allocation excluded; no forced GC; no universal memory, browser, socket or live-Hub claim.",
      },
      null,
      2,
    ),
  );
});
