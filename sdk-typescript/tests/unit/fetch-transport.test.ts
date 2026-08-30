import { describe, expect, it } from "vitest";
import { asEntityTag } from "../../src/core/opaque-values.js";
import {
  FetchTransport,
  InvalidBaseUrlError,
  InvalidRequestPathError,
  ReservedAuthorizationHeaderError,
  type FetchImplementation,
} from "../../src/http/fetch-transport.js";

describe("fetch transport", () => {
  it.each(["ftp://hub.example", "file:///tmp/hub", "https://user:secret@hub.example"])(
    "rejects unsafe base URL %s",
    (baseUrl) => {
      expect(() => new FetchTransport({ baseUrl, fetch: unusedFetch })).toThrow(
        InvalidBaseUrlError,
      );
    },
  );

  it.each(["https://other.example/v1/items", "//other.example/v1/items", "v1/items"])(
    "rejects non-root-relative request target %s",
    async (path) => {
      const transport = new FetchTransport({ baseUrl: "https://hub.example", fetch: unusedFetch });

      await expect(transport.request(path)).rejects.toThrow(InvalidRequestPathError);
    },
  );

  it("resolves a root-relative path on the configured origin", async () => {
    const inputs: string[] = [];
    const transport = new FetchTransport({
      baseUrl: "https://hub.example/base",
      fetch: async (input) => {
        inputs.push(String(input));
        return new Response(null, { status: 204 });
      },
    });

    await transport.request("/v1/items?limit=20");

    expect(inputs).toEqual(["https://hub.example/v1/items?limit=20"]);
  });

  it("reserves Authorization for the caller provider", async () => {
    const transport = new FetchTransport({ baseUrl: "https://hub.example", fetch: unusedFetch });

    await expect(
      transport.request("/v1/items", { headers: { Authorization: "Bearer secret" } }),
    ).rejects.toThrow(ReservedAuthorizationHeaderError);
  });

  it("loads authorization for each request without retaining the previous value", async () => {
    const seen: Array<string | null> = [];
    const values = ["Bearer first", "Bearer second"];
    const transport = new FetchTransport({
      baseUrl: "https://hub.example",
      authorization: () => values.shift(),
      fetch: async (_input, init) => {
        seen.push(new Headers(init?.headers).get("authorization"));
        return new Response(null, { status: 204 });
      },
    });

    await transport.request("/v1/items");
    await transport.request("/v1/items");

    expect(seen).toEqual(["Bearer first", "Bearer second"]);
  });

  it("applies an entity tag and makes one request for an HTTP failure", async () => {
    let calls = 0;
    let seenTag: string | null = null;
    const transport = new FetchTransport({
      baseUrl: "https://hub.example",
      fetch: async (_input, init) => {
        calls += 1;
        seenTag = new Headers(init?.headers).get("if-none-match");
        return new Response("unavailable", { status: 503 });
      },
    });

    const response = await transport.request("/v1/items", {
      ifNoneMatch: asEntityTag('W/"revision-7"'),
    });

    expect(response.status).toBe(503);
    expect(seenTag).toBe('W/"revision-7"');
    expect(calls).toBe(1);
  });

  it("normalizes network failure without leaking credential-bearing causes", async () => {
    let calls = 0;
    const transport = new FetchTransport({
      baseUrl: "https://hub.example",
      authorization: () => "Bearer secret-value",
      fetch: async () => {
        calls += 1;
        throw new Error("network failed with Bearer secret-value");
      },
    });

    const error = await captureError(transport.request("/v1/items"));

    expect(error).toMatchObject({
      name: "TransportError",
      message: "Teslatlas transport request failed",
    });
    expect(error).not.toHaveProperty("cause");
    expect(String(error)).not.toContain("secret-value");
    expect(calls).toBe(1);
  });

  it("propagates an abort reason without retrying", async () => {
    const controller = new AbortController();
    const reason = new DOMException("Stopped", "AbortError");
    controller.abort(reason);
    let calls = 0;
    const transport = new FetchTransport({
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
});

const unusedFetch: FetchImplementation = async () => new Response(null, { status: 204 });

async function captureError(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
    return undefined;
  } catch (error) {
    return error;
  }
}
