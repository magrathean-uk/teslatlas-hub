import { describe, expect, it } from "vitest";
import { asEntityTag } from "../../src/core/opaque-values.js";
import type {
  FetchImplementation,
  FetchTransport,
  FetchTransportOptions,
} from "../../src/http/fetch-transport.js";
import type { subscribeToSse } from "../../src/events/sse-subscription.js";

interface RuntimeSdk {
  readonly createTransport: (options: FetchTransportOptions) => FetchTransport;
  readonly subscribeToSse: typeof subscribeToSse;
}

export function defineTransportConformanceSuite(runtimeName: string, sdk: RuntimeSdk): void {
  describe(`${runtimeName} transport`, () => {
    it("creates a transport adapter", () => {
      expect(() =>
        sdk.createTransport({ baseUrl: "https://hub.example", fetch: successfulFetch }),
      ).not.toThrow();
    });

    it("preserves paths, queries, and conditional request tags", async () => {
      const observed: Array<{ url: string; headers: Headers }> = [];
      const transport = sdk.createTransport({
        baseUrl: "https://hub.example",
        fetch: async (input, init) => {
          observed.push({ url: String(input), headers: new Headers(init?.headers) });
          return new Response(null, { status: 304, headers: { ETag: '"revision-8"' } });
        },
      });

      const response = await transport.request("/v1/items?cursor=opaque%2B%2F%3D", {
        ifNoneMatch: asEntityTag('W/"revision-7"'),
      });

      expect(response.status).toBe(304);
      expect(observed).toHaveLength(1);
      expect(observed[0]?.url).toBe("https://hub.example/v1/items?cursor=opaque%2B%2F%3D");
      expect(observed[0]?.headers.get("if-none-match")).toBe('W/"revision-7"');
    });

    it("loads caller authorization and never retries an HTTP response", async () => {
      let calls = 0;
      let authorization: string | null = null;
      const transport = sdk.createTransport({
        baseUrl: "https://hub.example",
        authorization: () => "Bearer caller-owned",
        fetch: async (_input, init) => {
          calls += 1;
          authorization = new Headers(init?.headers).get("authorization");
          return new Response(null, { status: 503 });
        },
      });

      const response = await transport.request("/v1/items");

      expect(response.status).toBe(503);
      expect(authorization).toBe("Bearer caller-owned");
      expect(calls).toBe(1);
    });

    it("propagates abort without another fetch", async () => {
      const controller = new AbortController();
      const reason = new DOMException("Stopped", "AbortError");
      controller.abort(reason);
      let calls = 0;
      const transport = sdk.createTransport({
        baseUrl: "https://hub.example",
        fetch: async (_input, init) => {
          calls += 1;
          throw init?.signal?.reason;
        },
      });

      await expect(transport.request("/v1/items", { signal: controller.signal })).rejects.toBe(
        reason,
      );
      expect(calls).toBe(1);
    });

    it("parses one resumed SSE event through the runtime adapter", async () => {
      let lastEventId: string | null = null;
      const transport = sdk.createTransport({
        baseUrl: "https://hub.example",
        fetch: async (_input, init) => {
          lastEventId = new Headers(init?.headers).get("last-event-id");
          return new Response("id: event-8\ndata: first\ndata: second\n\n", {
            headers: { "Content-Type": "text/event-stream" },
          });
        },
      });

      const events = [];
      for await (const event of sdk.subscribeToSse({
        transport,
        path: "/events",
        checkpoint: {
          load: () => "event-7",
          save: () => undefined,
        },
      })) {
        events.push(event);
        break;
      }

      expect(lastEventId).toBe("event-7");
      expect(events).toEqual([
        {
          event: "message",
          data: "first\nsecond",
          lastEventId: "event-8",
        },
      ]);
    });
  });
}

const successfulFetch: FetchImplementation = async () => new Response(null, { status: 204 });
