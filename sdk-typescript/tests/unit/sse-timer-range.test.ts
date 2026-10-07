import { describe, expect, it, vi } from "vitest";
import { FetchTransport } from "../../src/http/fetch-transport.js";
import { subscribeToSse } from "../../src/events/sse-subscription.js";

const maximumTimerDelay = 2_147_483_647;
const overflowingDelay = maximumTimerDelay + 1;

describe("SSE default sleep timer range", () => {
  it.each(["server", "policy"] as const)(
    "waits the full selected %s delay in native-safe segments",
    async (source) => {
      const state = subscription(source, overflowingDelay);
      try {
        await vi.advanceTimersByTimeAsync(0);
        expect(state.fetch).toHaveBeenCalledTimes(1);
        expect(state.delays()).toEqual([maximumTimerDelay]);

        await vi.advanceTimersByTimeAsync(maximumTimerDelay);
        expect(state.fetch).toHaveBeenCalledTimes(1);
        expect(state.delays()).toEqual([maximumTimerDelay, 1]);

        await vi.advanceTimersByTimeAsync(1);
        await expect(state.pending).resolves.toMatchObject({
          done: false,
          value: { data: "resumed" },
        });
        expect(state.fetch).toHaveBeenCalledTimes(2);
        expect(vi.getTimerCount()).toBe(0);
      } finally {
        await state.dispose();
      }
    },
  );

  it.each(["first", "remaining"] as const)(
    "aborts during the %s segment without reconnecting or retaining its timer",
    async (segment) => {
      const state = subscription("server", overflowingDelay);
      try {
        await vi.advanceTimersByTimeAsync(0);
        expect(state.delays()).toEqual([maximumTimerDelay]);
        if (segment === "remaining") {
          await vi.advanceTimersByTimeAsync(maximumTimerDelay);
          expect(state.delays()).toEqual([maximumTimerDelay, 1]);
        }

        const reason = new Error("caller cancellation");
        state.controller.abort(reason);
        await expect(state.pending).rejects.toBe(reason);
        expect(state.fetch).toHaveBeenCalledTimes(1);
        expect(vi.getTimerCount()).toBe(0);
      } finally {
        await state.dispose();
      }
    },
  );

  it.each([0, 5, maximumTimerDelay])(
    "preserves an ordinary selected delay of %i milliseconds",
    async (delay) => {
      const state = subscription("policy", delay);
      try {
        await vi.advanceTimersByTimeAsync(0);
        if (delay === 0) {
          expect(state.delays()).toEqual([]);
        } else {
          expect(state.fetch).toHaveBeenCalledTimes(1);
          expect(state.delays()).toEqual([delay]);
          await vi.advanceTimersByTimeAsync(delay - 1);
          expect(state.fetch).toHaveBeenCalledTimes(1);
          await vi.advanceTimersByTimeAsync(1);
        }
        await expect(state.pending).resolves.toMatchObject({ value: { data: "resumed" } });
        expect(state.fetch).toHaveBeenCalledTimes(2);
        expect(vi.getTimerCount()).toBe(0);
      } finally {
        await state.dispose();
      }
    },
  );

  it("admits a maximum-safe policy delay while scheduling only its first finite segment", async () => {
    const state = subscription("policy", Number.MAX_SAFE_INTEGER);
    try {
      await vi.advanceTimersByTimeAsync(0);
      expect(state.fetch).toHaveBeenCalledTimes(1);
      expect(state.delays()).toEqual([maximumTimerDelay]);
      expect(vi.getTimerCount()).toBe(1);

      const reason = new Error("cancel very long wait");
      state.controller.abort(reason);
      await expect(state.pending).rejects.toBe(reason);
      expect(state.fetch).toHaveBeenCalledTimes(1);
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      await state.dispose();
    }
  });
});

function subscription(source: "server" | "policy", delay: number) {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  const timeout = vi.spyOn(globalThis, "setTimeout");
  const controller = new AbortController();
  let requests = 0;
  const fetch = vi.fn(async () => {
    requests += 1;
    if (requests > 2) throw new Error("unexpected reconnect");
    const body =
      requests === 1 ? (source === "server" ? `retry: ${delay}\n\n` : "") : "data: resumed\n\n";
    return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
  });
  const iterator = subscribeToSse({
    transport: new FetchTransport({ baseUrl: "https://example.invalid", fetch }),
    path: "/events",
    signal: controller.signal,
    maximumServerRetryMilliseconds: delay,
    reconnect: () => (source === "policy" ? delay : 0),
  })[Symbol.asyncIterator]();
  const pending = iterator.next();
  // Cleanup must remain safe if an earlier assertion fails before this settles.
  void pending.catch(() => undefined);
  return {
    controller,
    fetch,
    pending,
    delays: () => timeout.mock.calls.map(([, milliseconds]) => milliseconds),
    dispose: async () => {
      controller.abort();
      await pending.catch(() => undefined);
      await iterator.return?.();
      timeout.mockRestore();
      vi.useRealTimers();
    },
  };
}
