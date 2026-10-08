import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import { describe, expect, it } from "vitest";
import { createClientSession } from "../../src/client/session.js";
import { ProtocolValidationError } from "../../src/core/errors.js";
import { validateDiscovery } from "../../src/generated/validators.js";
import { InvalidBaseUrlError, type FetchImplementation } from "../../src/http/fetch-transport.js";
import type { HubDescriptor } from "../../src/protocol/models.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";

const canonicalDescriptor = decodeProtocolValue<HubDescriptor>(
  discovery,
  validateDiscovery,
  "validateDiscovery",
);

interface ObservedRequest {
  readonly url: string;
  readonly authorization: string | null;
}

describe("client session", () => {
  it("discovers without authorization and creates authenticated endpoint transports", async () => {
    const observed: ObservedRequest[] = [];
    const fetch: FetchImplementation = async (input, init) => {
      const url = String(input);
      observed.push({
        url,
        authorization: new Headers(init?.headers).get("authorization"),
      });
      if (url.endsWith("/.well-known/teslatlas-hub")) {
        return discoveryResponse();
      }
      return new Response(null, { status: 204 });
    };

    const session = await createClientSession({
      baseUrl: "https://hub.example.invalid",
      authorization: () => "Bearer redacted-test-value",
      fetch,
      requestedProtocolVersion: "1.2.0",
    });

    expect(session.protocolVersion).toBe("1.2.0");
    expect(session.descriptor.hub_id).toBe("urn:uuid:018f18d2-6f45-7b3c-8a91-3c7286a10d42");
    expect(observed[0]).toEqual({
      url: "https://hub.example.invalid/.well-known/teslatlas-hub",
      authorization: null,
    });

    await session.apiTransport.request("/v1/vehicles");
    await session.eventTransport.request("/v1/events");

    expect(observed.slice(1)).toEqual([
      {
        url: "https://hub.example.invalid/v1/vehicles",
        authorization: "Bearer redacted-test-value",
      },
      {
        url: "https://hub.example.invalid/v1/events",
        authorization: "Bearer redacted-test-value",
      },
    ]);
  });

  it("forwards the caller abort signal to discovery", async () => {
    const controller = new AbortController();
    let observedSignal: AbortSignal | null | undefined;
    const fetch: FetchImplementation = async (_input, init) => {
      observedSignal = init?.signal;
      return discoveryResponse();
    };

    await createClientSession({
      baseUrl: "https://hub.example.invalid",
      authorization: () => undefined,
      fetch,
      signal: controller.signal,
    });

    expect(observedSignal).toBe(controller.signal);
  });

  it.each(["http://hub.example.invalid", "http://user:secret@127.0.0.1:8787"])(
    "rejects unsafe bootstrap URL %s before fetching",
    async (baseUrl) => {
      let calls = 0;
      const error = await captureError(
        createClientSession({
          baseUrl,
          authorization: () => undefined,
          fetch: async () => {
            calls += 1;
            return Response.json(discovery);
          },
        }),
      );

      expect(error).toBeInstanceOf(InvalidBaseUrlError);
      expect(calls).toBe(0);
    },
  );

  it("rejects redirects for discovery", async () => {
    let observedRedirect: RequestRedirect | undefined;

    await createClientSession({
      baseUrl: "https://hub.example.invalid",
      authorization: () => undefined,
      fetch: async (_input, init) => {
        observedRedirect = init?.redirect;
        return discoveryResponse();
      },
    });

    expect(observedRedirect).toBe("error");
  });

  it.each([404, 500])("rejects schema-valid discovery with HTTP status %i", async (status) => {
    const error = await captureError(
      createClientSession({
        baseUrl: "https://hub.example.invalid",
        authorization: () => undefined,
        fetch: async () => Response.json(discovery, { status }),
      }),
    );

    expect(error).toBeInstanceOf(ProtocolValidationError);
    expect(error).toMatchObject({ validator: "Discovery.status" });
    expect(error).not.toHaveProperty("body");
    expect(error).not.toHaveProperty("response");
  });

  it("requires a valid ordinary ETag on discovery 200", async () => {
    const longEtag = `W/"${"x".repeat(513)}"`;
    const valid = await createClientSession({
      baseUrl: "https://hub.example.invalid",
      authorization: () => undefined,
      fetch: async () => Response.json(discovery, { headers: { ETag: longEtag } }),
    });

    expect(valid.descriptor).toEqual(canonicalDescriptor);

    const error = await captureError(
      createClientSession({
        baseUrl: "https://hub.example.invalid",
        authorization: () => undefined,
        fetch: async () =>
          Response.json(discovery, { headers: { "X-Debug-Secret": "Bearer must-not-leak" } }),
      }),
    );

    expect(error).toBeInstanceOf(ProtocolValidationError);
    expect(error).toMatchObject({ code: "protocol_validation", validator: "Discovery.etag" });
    expect(error).not.toHaveProperty("cause");
    expect(error).not.toHaveProperty("body");
    expect(error).not.toHaveProperty("response");
    expect(String(error)).not.toContain("must-not-leak");
  });

  it("rejects malformed discovery ETags without exposing the header", async () => {
    const error = await captureError(
      createClientSession({
        baseUrl: "https://hub.example.invalid",
        authorization: () => undefined,
        fetch: async () => Response.json(discovery, { headers: { ETag: "Bearer must-not-leak" } }),
      }),
    );

    expect(error).toBeInstanceOf(ProtocolValidationError);
    expect(error).toMatchObject({ code: "protocol_validation", validator: "Discovery.etag" });
    expect(error).not.toHaveProperty("cause");
    expect(error).not.toHaveProperty("headers");
    expect(String(error)).not.toContain("must-not-leak");
  });

  it("preserves caller abort while reading the discovery body", async () => {
    const controller = new AbortController();
    const reason = new DOMException("Stopped", "AbortError");
    const body = new ReadableStream({
      start(streamController) {
        controller.abort(reason);
        streamController.error(reason);
      },
    });

    const session = createClientSession({
      baseUrl: "https://hub.example.invalid",
      authorization: () => undefined,
      fetch: async () => new Response(body, { status: 200, headers: { ETag: 'W/"discovery-1"' } }),
      signal: controller.signal,
    });

    await expect(session).rejects.toBe(reason);
  });

  it("rejects malformed discovery without retaining payload fragments", async () => {
    const error = await captureError(
      createClientSession({
        baseUrl: "https://hub.example.invalid",
        authorization: () => undefined,
        fetch: async () =>
          Response.json(
            { secret: "Bearer must-not-leak" },
            { headers: { ETag: 'W/"discovery-1"' } },
          ),
      }),
    );

    expect(error).toBeInstanceOf(ProtocolValidationError);
    expect(error).toMatchObject({ validator: "validateDiscovery" });
    expect(error).not.toHaveProperty("cause");
    expect(String(error)).not.toContain("must-not-leak");
  });

  it("rejects non-HTTPS non-loopback discovered endpoints", async () => {
    const descriptor: HubDescriptor = {
      ...canonicalDescriptor,
      endpoints: {
        ...canonicalDescriptor.endpoints,
        api: "http://hub.example.invalid/v1",
      },
    };

    await expect(
      createClientSession({
        baseUrl: "https://hub.example.invalid",
        authorization: () => undefined,
        fetch: async () => discoveryResponse(descriptor),
      }),
    ).rejects.toMatchObject({
      name: "ProtocolValidationError",
      validator: "Discovery.endpoints",
    });
  });

  it("allows HTTP endpoints on an exact loopback host", async () => {
    const descriptor: HubDescriptor = {
      ...canonicalDescriptor,
      endpoints: {
        well_known: "http://127.0.0.1:8787/.well-known/teslatlas-hub",
        api: "http://127.0.0.1:8787/v1",
        events: "http://127.0.0.1:8787/v1/events",
        openapi: "http://127.0.0.1:8787/openapi/teslatlas-v1.openapi.json",
      },
    };

    const session = await createClientSession({
      baseUrl: "http://127.0.0.1:8787",
      authorization: () => undefined,
      fetch: async () => discoveryResponse(descriptor),
    });

    expect(session.descriptor.endpoints.api).toBe("http://127.0.0.1:8787/v1");
  });
});

async function captureError(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
    return undefined;
  } catch (error) {
    return error;
  }
}

function discoveryResponse(value: unknown = discovery): Response {
  return Response.json(value, { headers: { ETag: 'W/"discovery-1"' } });
}
