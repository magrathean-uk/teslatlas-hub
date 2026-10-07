import { describe, expect, it } from "vitest";
import hubDiscovery from "../../protocol/source/profiles/hub-http-v1/1.0.0/examples/discovery.json" with {
  type: "json",
};
import drives from "../../protocol/source/profiles/hub-http-v1/1.0.0/examples/drives.json" with {
  type: "json",
};
import { createClientSession } from "../../src/client/session.js";
import { createHubClient } from "../../src/hub/client.js";
import type { HubCredentialStore } from "../../src/hub/models.js";
import {
  decodeReadResponse,
  decodeProtocolProblemResponse,
} from "../../src/http/response-decoder.js";
import { cancelResponseBody, withResponseOwnership } from "../../src/http/response-ownership.js";

const hubId = hubDiscovery.hub_id;
const endpoint = "https://synthetic.invalid";
const credentials: HubCredentialStore = {
  load: () => ({ accessToken: "a".repeat(64), deviceId: hubId, expiresAtMs: Date.now() + 60_000 }),
  save: () => undefined,
  clear: () => undefined,
};

function pending<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function openBody(stallCancellation = false) {
  let canceled = 0;
  const stream = new ReadableStream<Uint8Array>(
    {
      cancel() {
        canceled += 1;
        return stallCancellation ? new Promise<void>(() => undefined) : undefined;
      },
    },
    { highWaterMark: 0 },
  );
  return { stream, cancellations: () => canceled };
}

describe("response lifecycle remediation", () => {
  it.each(["success", "problem"] as const)(
    "cancels a %s body rejected before JSON reading",
    async (kind) => {
      const body = openBody();
      const response = new Response(body.stream, {
        status: kind === "problem" ? 500 : 200,
        headers: { "Content-Type": "text/plain" },
      });
      const read =
        kind === "problem"
          ? decodeProtocolProblemResponse(response, undefined)
          : decodeReadResponse(response, () => true, "synthetic");
      await expect(read).rejects.toMatchObject({ code: "protocol_validation" });
      expect(body.cancellations()).toBe(1);
      expect(body.stream.locked).toBe(false);
    },
  );

  it.each(["status", "etag"] as const)(
    "cancels bootstrap discovery on rejected %s",
    async (kind) => {
      const body = openBody();
      await expect(
        createClientSession({
          baseUrl: endpoint,
          authorization: () => undefined,
          fetch: async () =>
            new Response(body.stream, {
              status: kind === "status" ? 500 : 200,
              headers: { "Content-Type": "application/json" },
            }),
        }),
      ).rejects.toMatchObject({
        validator: kind === "status" ? "Discovery.status" : "Discovery.etag",
      });
      expect(body.cancellations()).toBe(1);
    },
  );

  it("does not await a custom cancellation sink or replace the admission failure", async () => {
    const body = openBody(true);
    const response = new Response(body.stream);
    const original = new Error("synthetic failure");
    await expect(
      withResponseOwnership(response, async () => {
        throw original;
      }),
    ).rejects.toBe(original);
    expect(body.cancellations()).toBe(1);
    await cancelResponseBody(response);
  });

  it("leaves a successfully returned body under the consumer's ownership", async () => {
    const body = openBody();
    const response = new Response(body.stream);
    await expect(withResponseOwnership(response, async () => response)).resolves.toBe(response);
    expect(body.cancellations()).toBe(0);
    await body.stream.cancel();
  });

  it.each(["cache", "etag", "mime"] as const)(
    "cancels rejected Hub drive %s before reader acquisition",
    async (kind) => {
      const body = openBody();
      const headers = {
        "Cache-Control": kind === "cache" ? "public" : "no-store",
        ETag: kind === "etag" ? "invalid" : '"page"',
        "Content-Type": kind === "mime" ? "text/plain" : "application/json",
      };
      const client = createHubClient({
        endpoint,
        expectedHubId: hubId,
        credentials,
        fetch: async (input) =>
          String(input).includes(".well-known")
            ? Response.json(hubDiscovery)
            : new Response(body.stream, { headers }),
      });
      await expect(client.drives(hubId)).rejects.toMatchObject({ code: "protocol_validation" });
      expect(body.cancellations()).toBe(1);
      client.dispose();
    },
  );
});

describe("independent Hub discovery callers", () => {
  it.each(["owner abort", "logout", "dispose"] as const)(
    "cancels both shared waiters on %s",
    async (action) => {
      const owner = new AbortController();
      const entered = pending<void>();
      let calls = 0;
      let sharedSignal: AbortSignal | null | undefined;
      const client = createHubClient({
        endpoint,
        expectedHubId: hubId,
        credentials,
        signal: owner.signal,
        fetch: async (_input, init) => {
          calls += 1;
          sharedSignal = init?.signal;
          entered.resolve();
          return new Promise<Response>((_resolve, reject) =>
            init?.signal?.addEventListener("abort", () => reject(init.signal?.reason), {
              once: true,
            }),
          );
        },
      });
      const first = client.discover();
      const second = client.discover();
      const settled = Promise.allSettled([first, second]);
      await entered.promise;
      if (action === "owner abort") owner.abort();
      else if (action === "logout") await client.logout();
      else client.dispose();
      const results = await settled;
      for (const result of results)
        expect(result).toMatchObject({ status: "rejected", reason: { name: "AbortError" } });
      expect(sharedSignal?.aborted).toBe(true);
      expect(calls).toBe(1);
      client.dispose();
    },
  );
  it.each(["first", "second"] as const)(
    "rejects only the aborted %s caller while coalescing one fetch",
    async (aborted) => {
      const response = pending<Response>();
      const entered = pending<void>();
      let calls = 0;
      let transportSignal: AbortSignal | null | undefined;
      const client = createHubClient({
        endpoint,
        expectedHubId: hubId,
        credentials,
        fetch: async (_input, init) => {
          calls += 1;
          transportSignal = init?.signal;
          entered.resolve();
          return response.promise;
        },
      });
      const a = new AbortController();
      const b = new AbortController();
      const first = client.discover({ signal: a.signal });
      const second = client.discover({ signal: b.signal });
      const settled = Promise.allSettled([first, second]);
      await entered.promise;
      const reason = new Error("caller stopped");
      (aborted === "first" ? a : b).abort(reason);
      await expect(aborted === "first" ? first : second).rejects.toBe(reason);
      expect(transportSignal?.aborted).toBe(false);
      response.resolve(Response.json(hubDiscovery));
      const values = await settled;
      expect(values[aborted === "first" ? 0 : 1]).toMatchObject({ status: "rejected", reason });
      expect(values[aborted === "first" ? 1 : 0]).toMatchObject({
        status: "fulfilled",
        value: { value: { hubId } },
      });
      expect(calls).toBe(1);
      client.dispose();
    },
  );

  it("aborts shared work when its last caller leaves and permits a fresh request", async () => {
    const entered = pending<void>();
    let calls = 0;
    let firstSignal: AbortSignal | null | undefined;
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: async (_input, init) => {
        calls += 1;
        if (calls > 1) return Response.json(hubDiscovery);
        firstSignal = init?.signal;
        entered.resolve();
        return new Promise<Response>((_resolve, reject) =>
          init?.signal?.addEventListener("abort", () => reject(init.signal?.reason), {
            once: true,
          }),
        );
      },
    });
    const controller = new AbortController();
    const first = client.discover({ signal: controller.signal });
    const rejection = expect(first).rejects.toMatchObject({ name: "AbortError" });
    await entered.promise;
    controller.abort();
    await rejection;
    expect(firstSignal?.aborted).toBe(true);
    await expect(client.discover()).resolves.toMatchObject({ value: { hubId } });
    expect(calls).toBe(2);
    client.dispose();
  });
});

describe("Current Hub independent header and identity rules", () => {
  it.each(["logout", "dispose"] as const)(
    "stops bearer dispatch when %s follows credential admission",
    async (action) => {
      let calls = 0;
      let loads = 0;
      let stopped = false;
      const client = createHubClient({
        endpoint,
        expectedHubId: hubId,
        credentials: {
          ...credentials,
          load: () => {
            loads += 1;
            queueMicrotask(() =>
              queueMicrotask(() => {
                stopped = true;
                if (action === "logout") void client.logout();
                else client.dispose();
              }),
            );
            return {
              accessToken: "a".repeat(64),
              deviceId: hubId,
              expiresAtMs: Date.now() + 60_000,
            };
          },
        },
        fetch: async () => {
          calls += 1;
          return Response.json(hubDiscovery);
        },
      });
      await expect(client.vehicles()).rejects.toMatchObject({ name: "AbortError" });
      expect(stopped).toBe(true);
      expect(loads).toBe(1);
      expect(calls).toBe(1);
      client.dispose();
    },
  );
  it.each([514, 65_536])("round trips a strong %i-character Hub ETag", async (length) => {
    const etag = `"${"x".repeat(length - 2)}"`;
    const observed: Array<string | null> = [];
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: async (input, init) => {
        if (String(input).includes(".well-known")) return Response.json(hubDiscovery);
        observed.push(new Headers(init?.headers).get("If-None-Match"));
        return observed.length === 1
          ? Response.json(drives, { headers: { ETag: etag, "Cache-Control": "no-store" } })
          : new Response(null, {
              status: 304,
              headers: { ETag: etag, "Cache-Control": "no-store" },
            });
      },
    });
    const page = await client.drives(hubId);
    const issuedTag = page.metadata.etag;
    if (issuedTag === undefined) throw new Error("expected issued Hub ETag");
    await expect(client.drives(hubId, { ifNoneMatch: issuedTag })).resolves.toMatchObject({
      kind: "notModified",
      metadata: { etag },
    });
    expect(observed).toEqual([null, etag]);
    client.dispose();
  });

  it("rejects a Hub ETag above its profile budget before authentication", async () => {
    let calls = 0;
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: async () => {
        calls += 1;
        return Response.json(hubDiscovery);
      },
    });
    await expect(
      client.drives(hubId, { ifNoneMatch: `"${"x".repeat(65_535)}"` as never }),
    ).rejects.toMatchObject({ code: "invalid_strong_entity_tag" });
    expect(calls).toBe(0);
    client.dispose();
  });

  it("accepts the packaged lowercase UUID shape including version seven", async () => {
    const id = "018f18d2-6f45-7b3c-8a91-3c7286a10d42";
    const client = createHubClient({
      endpoint,
      expectedHubId: id,
      credentials,
      fetch: async () => Response.json({ ...hubDiscovery, hub_id: id }),
    });
    await expect(client.discover()).resolves.toMatchObject({ value: { hubId: id } });
    client.dispose();
  });

  it("rejects an expired stored credential before bearer dispatch", async () => {
    const requests: Array<string | null> = [];
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials: {
        ...credentials,
        load: () => ({ accessToken: "a".repeat(64), deviceId: hubId, expiresAtMs: Date.now() - 1 }),
      },
      fetch: async (_input, init) => {
        requests.push(new Headers(init?.headers).get("Authorization"));
        return Response.json(hubDiscovery);
      },
    });
    await expect(client.vehicles()).rejects.toMatchObject({ validator: "HubCredential.expiry" });
    expect(requests).toEqual([null]);
    client.dispose();
  });
});
