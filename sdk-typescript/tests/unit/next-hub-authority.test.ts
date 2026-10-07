import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";
import { createHubClient } from "../../src/hub/client.js";
import type { HubCredential, HubDrivesOptions } from "../../src/hub/models.js";
import { asStrongEntityTag } from "../../src/http/strong-etag.js";

const hubId = "11111111-1111-4111-8111-111111111111";
const otherId = "22222222-2222-4222-8222-222222222222";
const endpoint = "https://hub.example.invalid";
const root = new URL("../../protocol/source/profiles/hub-http-v1/1.0.0/examples/", import.meta.url);
async function fixture(name: string) {
  return JSON.parse(await readFile(new URL(`${name}.json`, root), "utf8"));
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
const headers = { "Content-Type": "application/json", "Cache-Control": "no-store", ETag: '"page"' };
const credential: HubCredential = {
  deviceId: hubId,
  accessToken: "a".repeat(64),
  expiresAtMs: 1_900_000_000_000,
};

describe("CurrentHub immutable authority", () => {
  it.each([
    [false, "widen"],
    [false, "narrow"],
    [true, "widen"],
  ] as const)(
    "admits against issued options despite caller mutation (bad page=%s, %s)",
    async (bad, mutation) => {
      const pending = deferred<Response>();
      const entered = deferred<Request>();
      const page = await fixture("drives");
      if (bad) page.items[0].start_date_ms += 1_000_000;
      const client = createHubClient({
        endpoint,
        expectedHubId: hubId,
        credentials: { load: () => credential, save: () => {}, clear: () => {} },
        fetch: async (input, init) => {
          const request = new Request(input, init);
          if (new URL(request.url).pathname.startsWith("/.well-known"))
            return Response.json(await fixture("discovery"));
          entered.resolve(request);
          return pending.promise;
        },
      });
      const options = {
        fromMs: 1_788_565_000_000,
        toMs: 1_788_566_000_000,
        limit: 1,
        cursor: "issued",
        ifNoneMatch: asStrongEntityTag('"page"'),
      };
      const result = client.drives(hubId, options);
      const request = await entered.promise;
      options.fromMs = mutation === "widen" ? 0 : 1_900_000_000_000;
      options.toMs = Number.MAX_SAFE_INTEGER;
      options.limit = mutation === "widen" ? 100 : 0;
      options.cursor = "changed";
      options.ifNoneMatch = asStrongEntityTag('"changed"');
      pending.resolve(Response.json(page, { headers }));
      expect(new URL(request.url).searchParams.get("cursor")).toBe("issued");
      expect(request.headers.get("If-None-Match")).toBe('"page"');
      if (bad)
        await expect(result).rejects.toMatchObject({
          code: "protocol_validation",
          validator: "HubDrives.timeRange",
        });
      else
        await expect(result).resolves.toMatchObject({
          kind: "page",
          value: { items: [{ id: 101 }] },
        });
    },
  );

  it("reads accessor options once before discovery can mutate them", async () => {
    const accesses: Record<string, number> = {};
    const values: Record<string, unknown> = {
      fromMs: 1_788_565_000_000,
      toMs: 1_788_566_000_000,
      limit: 1,
      cursor: "issued",
    };
    const options = Object.fromEntries([]);
    for (const name of ["fromMs", "toMs", "limit", "cursor", "ifNoneMatch", "signal"]) {
      Object.defineProperty(options, name, {
        get() {
          accesses[name] = (accesses[name] ?? 0) + 1;
          return values[name];
        },
      });
    }
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials: { load: () => credential, save: () => {}, clear: () => {} },
      fetch: async (input) => {
        if (String(input).includes("well-known")) {
          values.fromMs = 1_900_000_000_000;
          return Response.json(await fixture("discovery"));
        }
        return Response.json(await fixture("drives"), { headers });
      },
    });
    await expect(client.drives(hubId, options as HubDrivesOptions)).resolves.toMatchObject({
      kind: "page",
    });
    expect(Object.values(accesses)).toEqual([1, 1, 1, 1, 1, 1]);
  });

  it("authenticates rotation with the single credential used to bind response identity", async () => {
    let loads = 0;
    let authorization: string | null = null;
    const saves: HubCredential[] = [];
    const returned = { ...credential };
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials: {
        load: () => {
          loads++;
          return loads === 1
            ? returned
            : { ...credential, deviceId: otherId, accessToken: "b".repeat(64) };
        },
        save: (value) => {
          saves.push(value);
        },
        clear: () => {},
      },
      fetch: async (input, init) => {
        const request = new Request(input, init);
        if (request.method === "GET") return Response.json(await fixture("discovery"));
        authorization = request.headers.get("Authorization");
        returned.deviceId = otherId;
        returned.accessToken = "c".repeat(64);
        return Response.json({
          device_id: hubId,
          access_token: "d".repeat(64),
          expires_at_ms: 1_900_000_000_000,
        });
      },
    });
    await expect(client.rotateDevice()).resolves.toMatchObject({ value: { deviceId: hubId } });
    expect(loads).toBe(1);
    expect(authorization).toBe(`Bearer ${credential.accessToken}`);
    expect(saves).toEqual([{ ...credential, accessToken: "d".repeat(64) }]);
  });

  it.each([undefined, '"other"', '"page"'])(
    "binds304 to the issued strong validator %s",
    async (tag) => {
      const client = createHubClient({
        endpoint,
        expectedHubId: hubId,
        credentials: { load: () => credential, save: () => {}, clear: () => {} },
        fetch: async (input) =>
          String(input).includes("well-known")
            ? Response.json(await fixture("discovery"))
            : new Response(null, { status: 304, headers }),
      });
      const result = client.drives(
        hubId,
        tag === undefined ? {} : { ifNoneMatch: asStrongEntityTag(tag) },
      );
      if (tag === '"page"') await expect(result).resolves.toMatchObject({ kind: "notModified" });
      else
        await expect(result).rejects.toMatchObject({
          code: "protocol_validation",
          validator: "HubDrives.304.condition",
        });
    },
  );
});
