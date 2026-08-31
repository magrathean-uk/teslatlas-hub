import commandJob from "../../protocol/source/examples/command-job.json" with { type: "json" };
import commandRequest from "../../protocol/source/examples/command-request.json" with {
  type: "json",
};
import currentState from "../../protocol/source/examples/current-state.json" with { type: "json" };
import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import drives from "../../protocol/source/examples/drives-page.json" with { type: "json" };
import metadataRecord from "../../protocol/source/examples/metadata-record.json" with {
  type: "json",
};
import metadataTombstone from "../../protocol/source/examples/metadata-tombstone.json" with {
  type: "json",
};
import problem from "../../protocol/source/examples/error.json" with { type: "json" };
import vehicles from "../../protocol/source/examples/vehicles-page.json" with { type: "json" };
import { describe, expect, it } from "vitest";
import type { TeslatlasClient } from "../../src/client/client.js";
import type { CreateClientOptions } from "../../src/client/types.js";
import { asIdempotencyKey } from "../../src/commands/idempotency.js";
import { asEntityTag } from "../../src/core/opaque-values.js";
import { validateCommandRequest, validateMetadataReplace } from "../../src/generated/validators.js";
import { asStrongEntityTag } from "../../src/http/strong-etag.js";
import type { FetchImplementation } from "../../src/http/fetch-transport.js";
import type { CommandRequest, MetadataReplace } from "../../src/protocol/models.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";

interface RuntimeClientSdk {
  readonly createClient: (options: CreateClientOptions) => Promise<TeslatlasClient>;
}

export function defineTypedClientConformanceSuite(
  runtimeName: string,
  sdk: RuntimeClientSdk,
): void {
  describe(`${runtimeName} typed client`, () => {
    it("discovers publicly, then performs a versioned authenticated vehicle read", async () => {
      const observed: Array<{
        path: string;
        authorization: string | null;
        version: string | null;
      }> = [];
      const client = await sdk.createClient(options(router(observed)));

      const freshDiscovery = await client.discoverHub();
      const vehiclePage = await client.listVehicles();

      expect(freshDiscovery.kind).toBe("modified");
      expect(vehiclePage.kind).toBe("modified");
      expect(observed.map(({ path }) => path)).toEqual([
        "/.well-known/teslatlas-hub",
        "/.well-known/teslatlas-hub",
        "/v1/vehicles",
      ]);
      expect(observed[1]).toMatchObject({ authorization: null, version: null });
      expect(observed[2]).toMatchObject({ authorization: "Bearer runtime", version: "1.2.0" });
    });

    it("maps current state 200 and 304", async () => {
      let currentCalls = 0;
      const client = await sdk.createClient(
        options(async (input) => {
          const path = new URL(String(input)).pathname;
          if (path === "/.well-known/teslatlas-hub") return Response.json(discovery);
          currentCalls += 1;
          if (currentCalls === 1) {
            return Response.json(currentState, { headers: { ETag: 'W/"current-1"' } });
          }
          return new Response(null, { status: 304, headers: { ETag: 'W/"current-1"' } });
        }),
      );

      await expect(client.getVehicleCurrentState("vehicle_demo_alpha")).resolves.toMatchObject({
        kind: "modified",
      });
      await expect(
        client.getVehicleCurrentState("vehicle_demo_alpha", {
          ifNoneMatch: asEntityTag('W/"current-1"'),
        }),
      ).resolves.toEqual({
        kind: "not-modified",
        metadata: { status: 304, etag: 'W/"current-1"' },
      });
    });

    it("preserves history pagination cursor bytes", async () => {
      const paths: string[] = [];
      let historyCalls = 0;
      const client = await sdk.createClient(
        options(async (input) => {
          const url = new URL(String(input));
          if (url.pathname === "/.well-known/teslatlas-hub") return Response.json(discovery);
          paths.push(`${url.pathname}${url.search}`);
          historyCalls += 1;
          return Response.json({
            ...drives,
            items: historyCalls === 1 ? drives.items : [],
            next_cursor: historyCalls === 1 ? "opaque_cursor_0002" : null,
          });
        }),
      );

      const first = await client.listVehicleDrives("vehicle_demo_alpha");
      expect(first.kind).toBe("modified");
      if (first.kind !== "modified") return;
      await client.listVehicleDrives("vehicle_demo_alpha", {
        cursor: first.value.next_cursor as never,
      });

      expect(paths).toEqual([
        "/v1/vehicles/vehicle_demo_alpha/drives",
        "/v1/vehicles/vehicle_demo_alpha/drives?cursor=opaque_cursor_0002",
      ]);
    });

    it("decodes a protocol problem into safe fields", async () => {
      const client = await sdk.createClient(
        options(async (input) => {
          const path = new URL(String(input)).pathname;
          if (path === "/.well-known/teslatlas-hub") return Response.json(discovery);
          return Response.json(problem, {
            status: 400,
            headers: { "Content-Type": "application/problem+json" },
          });
        }),
      );

      await expect(client.listVehicles()).rejects.toMatchObject({
        name: "ProtocolHttpError",
        code: "invalid_cursor",
        status: 400,
      });
    });

    it("runs the 1.1 command-idempotency receipt through the typed method", async () => {
      const observed: Array<{ method: string; path: string; version: string | null }> = [];
      const client = await sdk.createClient(
        options(async (input, init) => {
          const url = new URL(String(input));
          if (url.pathname === "/.well-known/teslatlas-hub") return Response.json(discovery);
          observed.push({
            method: init?.method ?? "GET",
            path: `${url.pathname}${url.search}`,
            version: new Headers(init?.headers).get("teslatlas-protocol-version"),
          });
          return Response.json(commandJob, {
            status: 202,
            headers: {
              ETag: '"command-1"',
              Location: "/v1/commands/command_demo_0001",
            },
          });
        }, "1.1.0"),
      );

      const result = await client.createCommand(
        decodeProtocolValue<CommandRequest>(
          commandRequest,
          validateCommandRequest,
          "validateCommandRequest",
        ),
        { idempotencyKey: asIdempotencyKey("11111111-1111-4111-8111-111111111111") },
      );

      expect(result).toMatchObject({
        value: { command_id: "command_demo_0001" },
        metadata: { status: 202, etag: '"command-1"' },
      });
      expect(observed).toEqual([{ method: "POST", path: "/v1/commands", version: "1.1.0" }]);
    });

    it("runs the 1.2 metadata If-Match transcript through typed methods", async () => {
      const observed: Array<{ method: string; path: string; ifMatch: string | null }> = [];
      const client = await sdk.createClient(
        options(async (input, init) => {
          const url = new URL(String(input));
          if (url.pathname === "/.well-known/teslatlas-hub") return Response.json(discovery);
          observed.push({
            method: init?.method ?? "GET",
            path: `${url.pathname}${url.search}`,
            ifMatch: new Headers(init?.headers).get("if-match"),
          });
          if (init?.method === "DELETE") {
            return Response.json(metadataTombstone, { headers: { ETag: '"metadata-3"' } });
          }
          return Response.json(metadataRecord, { headers: { ETag: '"metadata-1"' } });
        }),
      );
      const replacement = decodeProtocolValue<MetadataReplace>(
        { value: { text: "Updated redacted demonstration trip." } },
        validateMetadataReplace,
        "validateMetadataReplace",
      );

      await client.getMetadata("metadata_demo_note_0001");
      await client.replaceMetadata("metadata_demo_note_0001", replacement, {
        ifMatch: asStrongEntityTag('"metadata-1"'),
      });
      const deleted = await client.deleteMetadata("metadata_demo_note_0001", {
        ifMatch: asStrongEntityTag('"metadata-2"'),
      });

      expect(deleted).toMatchObject({ value: { metadata_id: "metadata_demo_note_0001" } });
      expect(observed).toEqual([
        { method: "GET", path: "/v1/metadata/metadata_demo_note_0001", ifMatch: null },
        { method: "PUT", path: "/v1/metadata/metadata_demo_note_0001", ifMatch: '"metadata-1"' },
        { method: "DELETE", path: "/v1/metadata/metadata_demo_note_0001", ifMatch: '"metadata-2"' },
      ]);
    });
  });
}

function options(
  fetch: FetchImplementation,
  requestedProtocolVersion: "1.0.0" | "1.1.0" | "1.2.0" = "1.2.0",
): CreateClientOptions {
  return {
    baseUrl: "https://hub.example.invalid",
    authorization: () => "Bearer runtime",
    requestedProtocolVersion,
    fetch,
  };
}

function router(
  observed: Array<{ path: string; authorization: string | null; version: string | null }>,
): FetchImplementation {
  return async (input, init) => {
    const url = new URL(String(input));
    observed.push({
      path: `${url.pathname}${url.search}`,
      authorization: new Headers(init?.headers).get("authorization"),
      version: new Headers(init?.headers).get("teslatlas-protocol-version"),
    });
    if (url.pathname === "/.well-known/teslatlas-hub") return Response.json(discovery);
    if (url.pathname === "/v1/vehicles") return Response.json(vehicles);
    throw new Error(`Unhandled conformance path ${url.pathname}`);
  };
}
