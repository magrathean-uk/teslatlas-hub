import { describe, expect, it, vi } from "vitest";
import {
  maximumJsonResponseBytes,
  readBoundedJson,
  readBoundedText,
} from "../../src/http/bounded-json.js";
import { createClientSession } from "../../src/client/session.js";
import { decodeReadResponse } from "../../src/http/response-decoder.js";
import { validateCurrentState } from "../../src/generated/validators.js";

describe("bounded rich JSON responses", () => {
  it("accepts the exact byte bound across chunks", async () => {
    const chunk = new TextEncoder().encode(`"${"a".repeat(maximumJsonResponseBytes - 2)}"`);
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(chunk.subarray(0, 11));
        controller.enqueue(chunk.subarray(11));
        controller.close();
      },
    });
    const value = await readBoundedJson(new Response(stream), undefined, "test");
    expect((value as string).length).toBe(maximumJsonResponseBytes - 2);
  });

  it.each(["discovery", "response"])(
    "bounds chunked %s even with a false length and cancels upstream",
    async (route) => {
      let cancelled = 0;
      let calls = 0;
      const stream = new ReadableStream<Uint8Array>({
        pull(controller) {
          calls += 1;
          controller.enqueue(new Uint8Array(1_024 * 1_024));
        },
        cancel() {
          cancelled += 1;
        },
      });
      const response = new Response(stream, {
        headers: { ETag: '"test"', "Content-Type": "application/json", "Content-Length": "1" },
      });
      const result =
        route === "discovery"
          ? createClientSession({
              baseUrl: "https://hub.example.invalid",
              authorization: () => undefined,
              fetch: async () => response,
            })
          : decodeReadResponse(response, validateCurrentState, "validateCurrentState");
      await expect(result).rejects.toMatchObject({
        code: "protocol_validation",
        validator: route === "discovery" ? "validateDiscovery.size" : "validateCurrentState.size",
      });
      expect(cancelled).toBe(1);
      expect(calls).toBeLessThanOrEqual(18);
    },
  );

  it("abort interrupts a stalled reader and preserves the caller reason", async () => {
    let cancelled = false;
    const response = new Response(
      new ReadableStream<Uint8Array>({
        cancel() {
          cancelled = true;
        },
      }),
    );
    const controller = new AbortController();
    const reason = new Error("caller cancelled");
    const pending = readBoundedJson(response, controller.signal, "test");
    controller.abort(reason);
    await expect(pending).rejects.toBe(reason);
    expect(cancelled).toBe(true);
  });
  it.each(["rich JSON", "current Hub text"])(
    "keeps cancellation bookkeeping bounded for heavily fragmented %s",
    async (kind) => {
      const controller = new AbortController();
      const add = vi.spyOn(controller.signal, "addEventListener");
      const remove = vi.spyOn(controller.signal, "removeEventListener");
      const race = vi.spyOn(Promise, "race");
      let offset = 0;
      const bytes = new TextEncoder().encode(`"${"a".repeat(20_000)}"`);
      const response = new Response(
        new ReadableStream<Uint8Array>({
          pull(stream) {
            if (offset === bytes.length) stream.close();
            else stream.enqueue(bytes.subarray(offset, ++offset));
          },
        }),
      );
      try {
        const result =
          kind === "rich JSON"
            ? await readBoundedJson(response, controller.signal, "test")
            : await readBoundedText(response, controller.signal, "test", 1_048_576);
        expect(result).toBe(
          kind === "rich JSON" ? "a".repeat(20_000) : new TextDecoder().decode(bytes),
        );
        expect(add).toHaveBeenCalledTimes(1);
        expect(remove).toHaveBeenCalledTimes(1);
        expect(remove.mock.calls[0]?.[1]).toBe(add.mock.calls[0]?.[1]);
        expect(race).not.toHaveBeenCalled();
        expect(response.body?.locked).toBe(false);
      } finally {
        race.mockRestore();
        add.mockRestore();
        remove.mockRestore();
      }
    },
  );

  it("abort completes even when upstream cancellation never settles", async () => {
    const controller = new AbortController();
    const response = new Response(
      new ReadableStream<Uint8Array>({
        cancel() {
          return new Promise<void>(() => {});
        },
      }),
    );
    const reason = new Error("caller cancelled stalled upstream");
    const pending = readBoundedJson(response, controller.signal, "test");
    controller.abort(reason);
    await expect(pending).rejects.toBe(reason);
    expect(response.body?.locked).toBe(false);
  });
  it("cancels unread upstream for an already-aborted caller", async () => {
    let cancelled = false;
    const response = new Response(
      new ReadableStream<Uint8Array>({
        cancel() {
          cancelled = true;
        },
      }),
    );
    const controller = new AbortController();
    const reason = new Error("already cancelled");
    controller.abort(reason);
    await expect(readBoundedJson(response, controller.signal, "test")).rejects.toBe(reason);
    expect(cancelled).toBe(true);
    expect(response.body?.locked).toBe(false);
  });
});
