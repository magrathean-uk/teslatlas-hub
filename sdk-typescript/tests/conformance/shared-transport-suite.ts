import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import eventEnvelope from "../../protocol/source/examples/event-envelope.json" with {
  type: "json",
};
import problem from "../../protocol/source/examples/error.json" with { type: "json" };
import { asEntityTag, asOpaqueCursor } from "../../src/index.js";
import type { CreateClientOptions, TeslatlasClient } from "../../src/index.js";
import { describe, expect, it } from "vitest";

type FetchImplementation = NonNullable<CreateClientOptions["fetch"]>;

interface RuntimeSdk {
  readonly createClient: (options: CreateClientOptions) => Promise<TeslatlasClient>;
}

export function defineTransportConformanceSuite(runtimeName: string, sdk: RuntimeSdk): void {
  describe(`${runtimeName} public client`, () => {
    it("creates a public client through the runtime entry", async () => {
      const client = await sdk.createClient({
        baseUrl: "https://hub.example",
        authorization: () => undefined,
        fetch: successfulFetch,
      });

      expect(client.protocolVersion).toBe("1.2.0");
    });

    it("preserves fixed operation paths, queries, and conditional request tags", async () => {
      const observed: Array<{ url: string; headers: Headers }> = [];
      const client = await sdk.createClient(
        options(async (input, init) => {
          const url = new URL(String(input));
          if (url.pathname === "/.well-known/teslatlas-hub") {
            return jsonGet(discovery, 'W/"discovery-1"');
          }
          observed.push({ url: String(input), headers: new Headers(init?.headers) });
          return new Response(null, { status: 304, headers: { ETag: 'W/"revision-8"' } });
        }),
      );

      const response = await client.listVehicles({
        cursor: asOpaqueCursor("opaque+/="),
        ifNoneMatch: asEntityTag('W/"revision-7"'),
      });

      expect(response).toEqual({
        kind: "not-modified",
        metadata: { status: 304, etag: 'W/"revision-8"' },
      });
      expect(observed).toHaveLength(1);
      expect(observed[0]?.url).toBe(
        "https://hub.example.invalid/v1/vehicles?cursor=opaque%2B%2F%3D",
      );
      expect(observed[0]?.headers.get("if-none-match")).toBe('W/"revision-7"');
      expect(observed[0]?.headers.get("teslatlas-protocol-version")).toBe("1.2.0");
    });

    it("loads caller authorization and never retries an HTTP response", async () => {
      let calls = 0;
      let authorization: string | null = null;
      const client = await sdk.createClient(
        options(async (input, init) => {
          const url = new URL(String(input));
          if (url.pathname === "/.well-known/teslatlas-hub") {
            return jsonGet(discovery, 'W/"discovery-1"');
          }
          calls += 1;
          authorization = new Headers(init?.headers).get("authorization");
          return Response.json(
            { ...problem, retryable: true, status: 503 },
            { status: 503, headers: { "Content-Type": "application/problem+json" } },
          );
        }),
      );

      await expect(client.listVehicles()).rejects.toMatchObject({
        name: "ProtocolHttpError",
        status: 503,
      });

      expect(authorization).toBe("Bearer caller-owned");
      expect(calls).toBe(1);
    });

    it("propagates abort through a first-class read without another fetch", async () => {
      const controller = new AbortController();
      const reason = new DOMException("Stopped", "AbortError");
      let calls = 0;
      const fetch: FetchImplementation = async (input, init) => {
        const url = new URL(String(input));
        if (url.pathname === "/.well-known/teslatlas-hub") {
          return jsonGet(discovery, 'W/"discovery-1"');
        }
        calls += 1;
        throw init?.signal?.reason;
      };
      const client = await sdk.createClient(options(fetch));
      controller.abort(reason);

      await expect(client.listVehicles({ signal: controller.signal })).rejects.toBe(reason);
      expect(calls).toBe(1);
    });

    it("streams one resumed validated event through the public client", async () => {
      let lastEventId: string | null = null;
      const client = await sdk.createClient(
        options(async (input, init) => {
          const url = new URL(String(input));
          if (url.pathname === "/.well-known/teslatlas-hub") {
            return jsonGet(discovery, 'W/"discovery-1"');
          }
          lastEventId = new Headers(init?.headers).get("last-event-id");
          return new Response(
            `id: ${eventEnvelope.event_id}\nevent: ${eventEnvelope.event_type}\ndata: ${JSON.stringify(eventEnvelope)}\n\n`,
            { headers: { "Content-Type": "text/event-stream" } },
          );
        }),
      );

      const iterator = client
        .streamEvents({
          checkpoint: {
            load: () => "event-7",
            save: () => undefined,
          },
        })
        [Symbol.asyncIterator]();
      await expect(iterator.next()).resolves.toMatchObject({ value: eventEnvelope, done: false });
      await iterator.return?.();

      expect(lastEventId).toBe("event-7");
    });
  });
}

function options(fetch: FetchImplementation): CreateClientOptions {
  return {
    baseUrl: "https://hub.example",
    authorization: () => "Bearer caller-owned",
    fetch,
  };
}

const successfulFetch: FetchImplementation = async (input) => {
  const url = new URL(String(input));
  if (url.pathname === "/.well-known/teslatlas-hub") {
    return jsonGet(discovery, 'W/"discovery-1"');
  }
  throw new Error(`Unexpected conformance path ${url.pathname}`);
};

function jsonGet(value: unknown, etag: string): Response {
  return Response.json(value, { headers: { ETag: etag } });
}
