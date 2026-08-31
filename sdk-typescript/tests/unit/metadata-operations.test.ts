import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import metadataPage from "../../protocol/source/examples/metadata-page.json" with { type: "json" };
import metadataRecord from "../../protocol/source/examples/metadata-record.json" with {
  type: "json",
};
import metadataTombstone from "../../protocol/source/examples/metadata-tombstone.json" with {
  type: "json",
};
import { describe, expect, it } from "vitest";
import { TeslatlasClient } from "../../src/client/client.js";
import type { ClientSession } from "../../src/client/types.js";
import { MissingCapabilityError, ProtocolValidationError } from "../../src/core/errors.js";
import { asEntityTag, asOpaqueCursor } from "../../src/core/opaque-values.js";
import {
  validateDiscovery,
  validateMetadataCreate,
  validateMetadataReplace,
} from "../../src/generated/validators.js";
import { InvalidRequestBodyError } from "../../src/http/request-builder.js";
import { asStrongEntityTag, type StrongEntityTag } from "../../src/http/strong-etag.js";
import { FetchTransport, type FetchImplementation } from "../../src/http/fetch-transport.js";
import type { HubDescriptor, MetadataCreate, MetadataReplace } from "../../src/protocol/models.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";

const descriptor = decodeProtocolValue<HubDescriptor>(discovery, validateDiscovery, "discovery");
const metadataCreate = decodeProtocolValue<MetadataCreate>(
  {
    vehicle_id: "vehicle_demo_alpha",
    kind: "note",
    target: { resource_type: "drive", resource_id: "drive_demo_0001" },
    value: { text: "Created redacted note." },
  },
  validateMetadataCreate,
  "validateMetadataCreate",
);
const metadataReplace = decodeProtocolValue<MetadataReplace>(
  { value: { text: "Updated redacted note." } },
  validateMetadataReplace,
  "validateMetadataReplace",
);

interface ObservedRequest {
  readonly method: string;
  readonly path: string;
  readonly headers: Headers;
  readonly body: string | null;
  readonly redirect: RequestRedirect | undefined;
}

describe("typed metadata operations", () => {
  it("uses the fixed metadata templates, headers, and bodies", async () => {
    const observed: ObservedRequest[] = [];
    const client = createClient(observed, async (input, init) => {
      const request = observe(input, init, observed);
      if (request.method === "GET" && request.path.startsWith("/v1/vehicles/")) {
        return Response.json(metadataPage, { headers: { ETag: 'W/"metadata-page-1"' } });
      }
      if (request.method === "POST") {
        return Response.json(metadataRecord, {
          status: 201,
          headers: {
            ETag: '"metadata-created-1"',
            Location: "/v1/metadata/metadata_demo_note_0001",
          },
        });
      }
      if (request.method === "GET") {
        return Response.json(metadataRecord, { headers: { ETag: '"metadata-read-1"' } });
      }
      if (request.method === "PUT") {
        return Response.json(metadataRecord, { headers: { ETag: '"metadata-replaced-1"' } });
      }
      if (request.method === "DELETE") {
        return Response.json(metadataTombstone, { headers: { ETag: '"metadata-deleted-1"' } });
      }
      throw new Error(`Unhandled request ${request.method} ${request.path}`);
    });

    const page = await client.listVehicleMetadata("vehicle/demo", {
      cursor: asOpaqueCursor("opaque_cursor_0001"),
      limit: 25,
      kind: "note",
      ifNoneMatch: asEntityTag('W/"metadata-page-1"'),
    });
    const created = await client.createMetadata("vehicle_demo_alpha", metadataCreate);
    const read = await client.getMetadata("metadata_demo_note_0001", {
      ifNoneMatch: asEntityTag('"metadata-read-1"'),
    });
    const replaced = await client.replaceMetadata("metadata_demo_note_0001", metadataReplace, {
      ifMatch: asStrongEntityTag('"metadata-read-1"'),
    });
    const deleted = await client.deleteMetadata("metadata_demo_note_0001", {
      ifMatch: asStrongEntityTag('"metadata-replaced-1"'),
    });

    expect(page).toMatchObject({ kind: "modified" });
    expect(created).toMatchObject({
      value: { metadata_id: "metadata_demo_note_0001" },
      metadata: {
        status: 201,
        etag: '"metadata-created-1"',
        location: "/v1/metadata/metadata_demo_note_0001",
      },
    });
    expect(read).toMatchObject({ kind: "modified" });
    expect(replaced).toMatchObject({ metadata: { status: 200, etag: '"metadata-replaced-1"' } });
    expect(deleted).toMatchObject({ metadata: { status: 200, etag: '"metadata-deleted-1"' } });

    expect(
      observed.map((request) => ({
        method: request.method,
        path: request.path,
        authorization: request.headers.get("authorization"),
        protocolVersion: request.headers.get("teslatlas-protocol-version"),
        ifNoneMatch: request.headers.get("if-none-match"),
        ifMatch: request.headers.get("if-match"),
        contentType: request.headers.get("content-type"),
        body: request.body,
        redirect: request.redirect,
      })),
    ).toEqual([
      {
        method: "GET",
        path: "/v1/vehicles/vehicle%2Fdemo/metadata?cursor=opaque_cursor_0001&limit=25&kind=note",
        authorization: "Bearer caller-owned",
        protocolVersion: "1.2.0",
        ifNoneMatch: 'W/"metadata-page-1"',
        ifMatch: null,
        contentType: null,
        body: null,
        redirect: "error",
      },
      {
        method: "POST",
        path: "/v1/vehicles/vehicle_demo_alpha/metadata",
        authorization: "Bearer caller-owned",
        protocolVersion: "1.2.0",
        ifNoneMatch: null,
        ifMatch: null,
        contentType: "application/json",
        body: JSON.stringify(metadataCreate),
        redirect: "error",
      },
      {
        method: "GET",
        path: "/v1/metadata/metadata_demo_note_0001",
        authorization: "Bearer caller-owned",
        protocolVersion: "1.2.0",
        ifNoneMatch: '"metadata-read-1"',
        ifMatch: null,
        contentType: null,
        body: null,
        redirect: "error",
      },
      {
        method: "PUT",
        path: "/v1/metadata/metadata_demo_note_0001",
        authorization: "Bearer caller-owned",
        protocolVersion: "1.2.0",
        ifNoneMatch: null,
        ifMatch: '"metadata-read-1"',
        contentType: "application/json",
        body: JSON.stringify(metadataReplace),
        redirect: "error",
      },
      {
        method: "DELETE",
        path: "/v1/metadata/metadata_demo_note_0001",
        authorization: "Bearer caller-owned",
        protocolVersion: "1.2.0",
        ifNoneMatch: null,
        ifMatch: '"metadata-replaced-1"',
        contentType: null,
        body: null,
        redirect: "error",
      },
    ]);
  });

  it("preserves a metadata kind without inventing a length limit", async () => {
    const observed: ObservedRequest[] = [];
    const kind = "k".repeat(129);
    const client = createClient(observed, async (input, init) => {
      observe(input, init, observed);
      return Response.json(metadataPage, { headers: { ETag: '"metadata-page-1"' } });
    });

    await expect(client.listVehicleMetadata("vehicle_demo_alpha", { kind })).resolves.toMatchObject(
      { kind: "modified" },
    );
    expect(observed).toHaveLength(1);
    expect(observed[0]?.path).toBe(`/v1/vehicles/vehicle_demo_alpha/metadata?kind=${kind}`);
  });

  it("requires an ordinary ETag on a metadata list 200", async () => {
    const clientWithEtag = createClient([], async () =>
      Response.json(metadataPage, { headers: { ETag: 'W/"metadata-page-1"' } }),
    );
    await expect(clientWithEtag.listVehicleMetadata("vehicle_demo_alpha")).resolves.toMatchObject({
      kind: "modified",
      metadata: { etag: 'W/"metadata-page-1"' },
    });

    const clientWithoutEtag = createClient([], async () => Response.json(metadataPage));
    await expect(
      clientWithoutEtag.listVehicleMetadata("vehicle_demo_alpha"),
    ).rejects.toBeInstanceOf(ProtocolValidationError);
  });

  it.each([
    [200, 'W/"metadata-weak-1"'],
    [304, 'W/"metadata-weak-1"'],
  ])("rejects a weak metadata entity ETag on %i", async (status, etag) => {
    const client = createClient([], async () =>
      status === 304
        ? new Response(null, { status, headers: { ETag: etag } })
        : Response.json(metadataRecord, { status, headers: { ETag: etag } }),
    );

    await expect(client.getMetadata("metadata_demo_note_0001")).rejects.toBeInstanceOf(
      ProtocolValidationError,
    );
  });

  it("maps a strong metadata 304 to not-modified", async () => {
    const client = createClient(
      [],
      async () => new Response(null, { status: 304, headers: { ETag: '"metadata-strong-1"' } }),
    );

    await expect(client.getMetadata("metadata_demo_note_0001")).resolves.toEqual({
      kind: "not-modified",
      metadata: { status: 304, etag: '"metadata-strong-1"' },
    });
  });

  it.each([200, 304])("accepts a long strong metadata response ETag on %i", async (status) => {
    const etag = `"${"x".repeat(512)}"`;
    const client = createClient([], async () =>
      status === 304
        ? new Response(null, { status, headers: { ETag: etag } })
        : Response.json(metadataRecord, { status, headers: { ETag: etag } }),
    );

    await expect(client.getMetadata("metadata_demo_note_0001")).resolves.toMatchObject({
      metadata: { status, etag },
    });
  });

  it("rejects unsafe created metadata Location values", async () => {
    const client = createClient([], async () =>
      Response.json(metadataRecord, {
        status: 201,
        headers: {
          ETag: '"metadata-created-1"',
          Location:
            "https://caller:secret@other.example.invalid/v1/metadata/metadata_demo_note_0001",
        },
      }),
    );

    await expect(
      client.createMetadata("vehicle_demo_alpha", metadataCreate),
    ).rejects.toBeInstanceOf(ProtocolValidationError);
  });

  it("rejects omitted or null metadata write options before authorization and Fetch", async () => {
    let authorizationCalls = 0;
    let fetchCalls = 0;
    const client = createClient(
      [],
      async () => {
        fetchCalls += 1;
        return Response.json(metadataRecord, {
          status: 201,
          headers: {
            ETag: '"metadata-created-1"',
            Location: "/v1/metadata/metadata_demo_note_0001",
          },
        });
      },
      descriptor,
      () => {
        authorizationCalls += 1;
        return "Bearer caller-owned";
      },
    );
    const replaceMetadata = client.replaceMetadata.bind(client) as unknown as (
      metadataId: string,
      body: MetadataReplace,
      options?: unknown,
    ) => Promise<unknown>;
    const deleteMetadata = client.deleteMetadata.bind(client) as unknown as (
      metadataId: string,
      options?: unknown,
    ) => Promise<unknown>;

    for (const invoke of [
      () => replaceMetadata("metadata_demo_note_0001", metadataReplace),
      () => replaceMetadata("metadata_demo_note_0001", metadataReplace, null),
      () => deleteMetadata("metadata_demo_note_0001"),
      () => deleteMetadata("metadata_demo_note_0001", null),
    ]) {
      const error = await captureError(invoke());
      expect(error).toMatchObject({ code: "invalid_strong_entity_tag" });
      expect(error).not.toBeInstanceOf(TypeError);
      expect(error).not.toHaveProperty("cause");
    }

    expect(authorizationCalls).toBe(0);
    expect(fetchCalls).toBe(0);
  });

  it("rejects lossy JSON metadata bodies before authorization and Fetch", async () => {
    let authorizationCalls = 0;
    let fetchCalls = 0;
    const client = createClient(
      [],
      async () => {
        fetchCalls += 1;
        return Response.json(metadataRecord, {
          status: 201,
          headers: {
            ETag: '"metadata-created-1"',
            Location: "/v1/metadata/metadata_demo_note_0001",
          },
        });
      },
      descriptor,
      () => {
        authorizationCalls += 1;
        return "Bearer caller-owned";
      },
    );
    const cycle: { self?: unknown } = {};
    cycle.self = cycle;
    const symbolKey = Symbol("metadata");
    const values: readonly unknown[] = [
      { nested: Number.NaN },
      { nested: Number.POSITIVE_INFINITY },
      { nested: Number.NEGATIVE_INFINITY },
      { nested: undefined },
      { nested: () => undefined },
      { nested: Symbol("metadata") },
      { [symbolKey]: "metadata" },
      { nested: 1n },
      cycle,
    ];

    for (const value of values) {
      const error = await captureError(
        client.createMetadata("vehicle_demo_alpha", { ...metadataCreate, value } as MetadataCreate),
      );
      expect(error).toBeInstanceOf(InvalidRequestBodyError);
      expect(error).toMatchObject({ code: "invalid_request_body" });
      expect(error).not.toHaveProperty("cause");
    }

    expect(authorizationCalls).toBe(0);
    expect(fetchCalls).toBe(0);
  });

  it("rejects invalid metadata inputs and absent capability before authorization or Fetch", async () => {
    let authorizationCalls = 0;
    let fetchCalls = 0;
    const fetch: FetchImplementation = async () => {
      fetchCalls += 1;
      return Response.json(metadataRecord);
    };
    const authorization = () => {
      authorizationCalls += 1;
      return "Bearer caller-owned";
    };
    const client = createClient([], fetch, descriptor, authorization);
    const noMetadataCapability: HubDescriptor = {
      ...descriptor,
      capabilities: descriptor.capabilities.filter(({ id }) => id !== "metadata.mutable"),
    };
    const noCapabilityClient = createClient([], fetch, noMetadataCapability, authorization);

    await expect(
      client.createMetadata("vehicle_demo_alpha", {
        ...metadataCreate,
        kind: "invalid-kind",
      } as MetadataCreate),
    ).rejects.toBeInstanceOf(ProtocolValidationError);
    await expect(
      client.replaceMetadata("metadata_demo_note_0001", metadataReplace, {
        ifMatch: 'W/"weak"' as StrongEntityTag,
      }),
    ).rejects.toMatchObject({ code: "invalid_strong_entity_tag" });
    await expect(
      noCapabilityClient.createMetadata("vehicle_demo_alpha", metadataCreate),
    ).rejects.toBeInstanceOf(MissingCapabilityError);

    expect(authorizationCalls).toBe(0);
    expect(fetchCalls).toBe(0);
  });
});

function createClient(
  observed: ObservedRequest[],
  fetch: FetchImplementation = async (input, init) => {
    const request = observe(input, init, observed);
    throw new Error(`Unhandled request ${request.method} ${request.path}`);
  },
  sessionDescriptor: HubDescriptor = descriptor,
  authorization: () => string = () => "Bearer caller-owned",
): TeslatlasClient {
  const session: ClientSession = {
    descriptor: sessionDescriptor,
    protocolVersion: "1.2.0",
    discoveryTransport: new FetchTransport({ baseUrl: "https://hub.example.invalid", fetch }),
    apiTransport: new FetchTransport({
      baseUrl: "https://api.example.invalid",
      authorization,
      fetch,
    }),
    eventTransport: new FetchTransport({ baseUrl: "https://events.example.invalid", fetch }),
  };
  return new TeslatlasClient(session);
}

function observe(
  input: RequestInfo | URL,
  init: RequestInit | undefined,
  observed: ObservedRequest[],
): ObservedRequest {
  const url = new URL(String(input));
  const request = {
    method: init?.method ?? "GET",
    path: `${url.pathname}${url.search}`,
    headers: new Headers(init?.headers),
    body: typeof init?.body === "string" ? init.body : null,
    redirect: init?.redirect,
  };
  observed.push(request);
  return request;
}

async function captureError(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
    return undefined;
  } catch (error) {
    return error;
  }
}
