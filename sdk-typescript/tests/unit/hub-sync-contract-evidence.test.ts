import { generateKeyPairSync, sign } from "node:crypto";
import { describe, expect, it, vi } from "vitest";
import {
  HUB_SYNC_SCHEMA_HEADER,
  requestSignedSyncJson,
  schema22SyncHeaders,
  validateSchema22Noop,
} from "../../scripts/hub-sync-contract-evidence.mjs";

describe("Hub sync contract evidence", () => {
  it("negotiates schema 2.2 on the manifest and verifies its exact bytes", async () => {
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    const publicDer = publicKey.export({ format: "der", type: "spki" });
    const publicKeyHex = publicDer.subarray(-32).toString("hex");
    const bytes = Buffer.from('{"schema":{"major":2,"minor":2}}\n');
    const signature = sign(null, bytes, privateKey).toString("base64");
    const fetch = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      expect(new Headers(init?.headers).get(HUB_SYNC_SCHEMA_HEADER)).toBe("2.2");
      expect(new Headers(init?.headers).get("authorization")).toBe("Bearer scoped");
      return new Response(bytes, {
        headers: {
          "cache-control": "no-store",
          "x-teslatlas-manifest-signature": signature,
        },
        status: 200,
      });
    });

    const result = await requestSignedSyncJson({
      authorization: { Authorization: "Bearer scoped" },
      endpoint: new URL("https://hub.example.invalid"),
      fetch,
      path: "/v1/vehicles/vehicle/sync/manifest",
      signaturePublicKey: publicKeyHex,
    });

    expect(fetch).toHaveBeenCalledOnce();
    expect(result).toMatchObject({
      body: { schema: { major: 2, minor: 2 } },
      signatureBytes: 64,
      signaturePresent: true,
      signatureVerified: true,
      status: 200,
    });

    const tamperedFetch = vi.fn(async () =>
      Promise.resolve(
        new Response(Buffer.from(`${bytes} `), {
          headers: { "x-teslatlas-manifest-signature": signature },
          status: 200,
        }),
      ),
    );
    await expect(
      requestSignedSyncJson({
        authorization: { Authorization: "Bearer scoped" },
        endpoint: new URL("https://hub.example.invalid"),
        fetch: tamperedFetch,
        path: "/v1/vehicles/vehicle/sync/manifest",
        signaturePublicKey: publicKeyHex,
      }),
    ).rejects.toThrow("exact body bytes");
  });

  it("adds negotiation without changing the caller's authorization object", () => {
    const authorization = { Authorization: "Bearer scoped" };
    expect(schema22SyncHeaders(authorization)).toEqual({
      Authorization: "Bearer scoped",
      "x-teslatlas-supported-schemas": "2.2",
    });
    expect(authorization).toEqual({ Authorization: "Bearer scoped" });
  });

  it.each([406, 503])("rejects an unavailable required no-op with HTTP %s", async (status) => {
    await expect(
      requestSignedSyncJson({
        authorization: { Authorization: "Bearer scoped" },
        endpoint: new URL("https://hub.example.invalid"),
        fetch: vi.fn(async () => new Response("unavailable", { status })),
        path: "/v1/vehicles/vehicle/sync/noop",
        signaturePublicKey: "00".repeat(32),
      }),
    ).rejects.toThrow(`returned HTTP ${status}`);
  });

  it("rejects a signed but malformed no-op body", async () => {
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    const publicDer = publicKey.export({ format: "der", type: "spki" });
    const bytes = Buffer.from("not-json");
    const signature = sign(null, bytes, privateKey).toString("base64");

    await expect(
      requestSignedSyncJson({
        authorization: { Authorization: "Bearer scoped" },
        endpoint: new URL("https://hub.example.invalid"),
        fetch: vi.fn(
          async () =>
            new Response(bytes, {
              headers: { "x-teslatlas-manifest-signature": signature },
              status: 200,
            }),
        ),
        path: "/v1/vehicles/vehicle/sync/noop",
        signaturePublicKey: publicDer.subarray(-32).toString("hex"),
      }),
    ).rejects.toThrow("invalid JSON");
  });

  it("rejects a required no-op with an invalid signature", async () => {
    const { publicKey } = generateKeyPairSync("ed25519");
    const publicDer = publicKey.export({ format: "der", type: "spki" });
    const bytes = Buffer.from(JSON.stringify(schema22Noop()));

    await expect(
      requestSignedSyncJson({
        authorization: { Authorization: "Bearer scoped" },
        endpoint: new URL("https://hub.example.invalid"),
        fetch: vi.fn(
          async () =>
            new Response(bytes, {
              headers: {
                "x-teslatlas-manifest-signature": Buffer.alloc(64).toString("base64"),
              },
              status: 200,
            }),
        ),
        path: "/v1/vehicles/vehicle/sync/noop",
        signaturePublicKey: publicDer.subarray(-32).toString("hex"),
      }),
    ).rejects.toThrow("exact body bytes");
  });

  it("rejects a validly signed JSON no-op that is missing its required contract", () => {
    expect(() => validateSchema22Noop(schema22Manifest(), {})).toThrow(
      "no-op response is malformed",
    );
  });

  it("accepts only a no-op bound to the required manifest", () => {
    const manifest = schema22Manifest();
    const noop = schema22Noop();
    expect(() => validateSchema22Noop(manifest, noop)).not.toThrow();
    expect(() =>
      validateSchema22Noop(manifest, { ...noop, head_sequence: noop.head_sequence + 1 }),
    ).toThrow("manifest/no-op pair does not match");
  });
});

function schema22Manifest() {
  return {
    account_id: "22222222-2222-4222-8222-222222222222",
    chunks: [{ sha256: "a".repeat(64) }],
    generation: 3,
    head_sequence: 7,
    installation_id: "11111111-1111-4111-8111-111111111111",
    snapshot_id: "44444444-4444-4444-8444-444444444444",
    terminal_cursor: "opaque-cursor",
    vehicle_id: "33333333-3333-4333-8333-333333333333",
  };
}

function schema22Noop() {
  return {
    account_id: "22222222-2222-4222-8222-222222222222",
    generation: 3,
    head_sequence: 7,
    installation_id: "11111111-1111-4111-8111-111111111111",
    pack_sha256: "a".repeat(64),
    projection_schema: "2.2",
    schema: "teslatlas-hub-schema-22-noop-v1",
    snapshot_id: "44444444-4444-4444-8444-444444444444",
    terminal_cursor: "opaque-cursor",
    vehicle_id: "33333333-3333-4333-8333-333333333333",
  };
}
