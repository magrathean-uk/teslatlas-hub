import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";
import type { HubCredential, HubCredentialStore } from "../../src/hub/models.js";
import { createHubClient } from "../../src/hub/client.js";
import { asStrongEntityTag } from "../../src/http/strong-etag.js";

const hubId = "11111111-1111-4111-8111-111111111111";
const otherHubId = "22222222-2222-4222-8222-222222222222";
const endpoint = "https://hub.example.invalid";
const jsonHeaders = { "Content-Type": "application/json", "X-Request-ID": "request-1" };
const profileRoot = new URL("../../protocol/source/profiles/hub-http-v1/1.0.0/", import.meta.url);

async function example(name: string): Promise<string> {
  return readFile(new URL(`examples/${name}.json`, profileRoot), "utf8");
}

class MemoryCredentials implements HubCredentialStore {
  credential: HubCredential | undefined;
  saves: HubCredential[] = [];
  clearCount = 0;

  async load(): Promise<HubCredential | undefined> {
    return this.credential;
  }

  async save(credential: HubCredential): Promise<void> {
    this.saves.push(credential);
    this.credential = credential;
  }

  async clear(): Promise<void> {
    this.clearCount += 1;
    this.credential = undefined;
  }
}

function queuedFetch(responses: Response[]) {
  const requests: Request[] = [];
  const fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    requests.push(new Request(input, init));
    const response = responses.shift();
    if (response === undefined) throw new Error("unexpected fetch");
    return response;
  };
  return { fetch, requests };
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function streamedResponseAt304(body: BodyInit, etag = '"drive-page"'): Response {
  const response = new Response(body, {
    headers: { ...jsonHeaders, ETag: etag, "Cache-Control": "no-store" },
  });
  Object.defineProperty(response, "status", { value: 304 });
  return response;
}

async function captureError(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
    return undefined;
  } catch (error) {
    return error;
  }
}

async function discoveryResponse(capabilities?: readonly string[]): Promise<Response> {
  const value = JSON.parse(await example("discovery")) as { capabilities: string[] };
  if (capabilities !== undefined) value.capabilities = [...capabilities];
  return new Response(JSON.stringify(value), { status: 200, headers: jsonHeaders });
}

describe("current Hub client", () => {
  it("discovers without credentials and accepts discovery without an ETag", async () => {
    const credentials = new MemoryCredentials();
    credentials.credential = {
      accessToken: "a".repeat(64),
      deviceId: hubId,
      expiresAtMs: 1_900_000_000_000,
    };
    const queue = queuedFetch([await discoveryResponse()]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });

    const result = await client.discover();

    expect(result.value.hubId).toBe(hubId);
    expect(result.metadata).toEqual({ status: 200, requestId: "request-1" });
    expect(queue.requests[0]?.headers.has("Authorization")).toBe(false);
    expect(queue.requests[0]?.redirect).toBe("error");
  });

  it("rejects a different Hub identity before sending credentials", async () => {
    const credentials = new MemoryCredentials();
    credentials.credential = {
      accessToken: "a".repeat(64),
      deviceId: hubId,
      expiresAtMs: 1_900_000_000_000,
    };
    const body = (await example("discovery")).replace(hubId, otherHubId);
    const queue = queuedFetch([new Response(body, { status: 200, headers: jsonHeaders })]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });

    await expect(client.vehicles()).rejects.toMatchObject({ code: "hub_identity_mismatch" });
    expect(queue.requests).toHaveLength(1);
    expect(queue.requests[0]?.headers.has("Authorization")).toBe(false);
  });

  it("gates current and drives independently before fetch", async () => {
    const queue = queuedFetch([
      await discoveryResponse(["query.vehicles", "query.current"]),
      new Response(await example("current"), { status: 200, headers: jsonHeaders }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials: new MemoryCredentials(),
      fetch: queue.fetch,
    });

    await expect(client.current(hubId)).resolves.toMatchObject({ value: { vehicleId: hubId } });
    await expect(client.drives(hubId)).rejects.toMatchObject({
      code: "unsupported_method",
      capability: "query.drives",
    });
    expect(queue.requests).toHaveLength(2);
  });

  it("sends exact millisecond drive filters and distinguishes page from notModified", async () => {
    const credentials = new MemoryCredentials();
    credentials.credential = {
      accessToken: "b".repeat(64),
      deviceId: hubId,
      expiresAtMs: 1_900_000_000_000,
    };
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(await example("drives"), {
        status: 200,
        headers: { ...jsonHeaders, ETag: '"drive-page"', "Cache-Control": "no-store" },
      }),
      new Response(null, {
        status: 304,
        headers: { ETag: '"drive-page"', "Cache-Control": "no-store" },
      }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });

    const page = await client.drives(hubId, {
      fromMs: 1_788_565_000_000,
      toMs: 1_788_566_000_000,
      limit: 1,
      cursor: "opaque+/=cursor",
    });
    const notModified = await client.drives(hubId, {
      ifNoneMatch: asStrongEntityTag('"drive-page"'),
    });

    expect(page.kind).toBe("page");
    expect(notModified).toEqual({
      kind: "notModified",
      metadata: { status: 304, etag: '"drive-page"' },
    });
    expect(queue.requests[1]?.url).toBe(
      `${endpoint}/v1/vehicles/${hubId}/drives?from_ms=1788565000000&to_ms=1788566000000&limit=1&cursor=opaque%2B%2F%3Dcursor`,
    );
    expect(queue.requests[1]?.headers.get("Authorization")).toBe(`Bearer ${"b".repeat(64)}`);
    expect(queue.requests[2]?.headers.get("If-None-Match")).toBe('"drive-page"');
  });

  it.each([
    [200, 'W/"weak"'],
    [200, "malformed"],
    [304, 'W/"weak"'],
    [304, "malformed"],
  ])("rejects a non-strong drives ETag on %i", async (status, etag) => {
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(status === 200 ? await example("drives") : null, {
        status,
        headers: { ...jsonHeaders, ETag: etag, "Cache-Control": "no-store" },
      }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials: new MemoryCredentials(),
      fetch: queue.fetch,
    });

    await expect(client.drives(hubId)).rejects.toMatchObject({ code: "protocol_validation" });
  });

  it.each(['W/"weak"', "malformed"])(
    "rejects a non-strong conditional drives ETag before fetch: %s",
    async (ifNoneMatch) => {
      const queue = queuedFetch([]);
      const client = createHubClient({
        endpoint,
        expectedHubId: hubId,
        credentials: new MemoryCredentials(),
        fetch: queue.fetch,
      });

      await expect(
        client.drives(hubId, { ifNoneMatch: ifNoneMatch as never }),
      ).rejects.toMatchObject({
        code: "invalid_strong_entity_tag",
      });
      expect(queue.requests).toHaveLength(0);
    },
  );

  it("accepts Chromium's empty 304 stream representation and rejects actual body bytes", async () => {
    const queue = queuedFetch([
      await discoveryResponse(),
      streamedResponseAt304(""),
      streamedResponseAt304(new Uint8Array([0xef, 0xbb, 0xbf])),
      streamedResponseAt304("unexpected"),
      streamedResponseAt304("x".repeat(1_048_577)),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials: new MemoryCredentials(),
      fetch: queue.fetch,
    });

    await expect(client.drives(hubId)).resolves.toEqual({
      kind: "notModified",
      metadata: { status: 304, etag: '"drive-page"', requestId: "request-1" },
    });
    await expect(client.drives(hubId)).rejects.toMatchObject({
      code: "protocol_validation",
      validator: "HubDrives.304",
    });
    await expect(client.drives(hubId)).rejects.toMatchObject({
      code: "protocol_validation",
      validator: "HubDrives.304",
    });
    await expect(client.drives(hubId)).rejects.toMatchObject({
      code: "protocol_validation",
      validator: "HubDrives.304",
    });
  });

  it("validates the vehicle identity in current and drive responses", async () => {
    const current = (await example("current")).replace(hubId, otherHubId);
    const drives = (await example("drives")).replace(hubId, otherHubId);
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(current, { status: 200, headers: jsonHeaders }),
      new Response(drives, {
        status: 200,
        headers: { ...jsonHeaders, ETag: '"drives"', "Cache-Control": "no-store" },
      }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials: new MemoryCredentials(),
      fetch: queue.fetch,
    });

    await expect(client.current(hubId)).rejects.toMatchObject({
      code: "vehicle_identity_mismatch",
    });
    await expect(client.drives(hubId)).rejects.toMatchObject({ code: "vehicle_identity_mismatch" });
  });

  it("handles typed and bare Hub errors without accepting an unrelated code", async () => {
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(JSON.stringify({ error: { code: "invalid_limit", message: "bad limit" } }), {
        status: 400,
        headers: jsonHeaders,
      }),
      new Response(null, { status: 401 }),
      new Response(JSON.stringify({ error: { code: "invalid_cursor", message: "wrong" } }), {
        status: 503,
        headers: jsonHeaders,
      }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials: new MemoryCredentials(),
      fetch: queue.fetch,
    });

    await expect(client.drives(hubId, { limit: 0 as never })).rejects.toMatchObject({
      code: "invalid_limit",
      status: 400,
      message: "bad limit",
    });
    await expect(client.vehicles()).rejects.toMatchObject({ code: "hub_http_error", status: 401 });
    await expect(client.drives(hubId)).rejects.toMatchObject({ code: "protocol_validation" });
  });

  it("claims and rotates by saving only fully validated credentials", async () => {
    const credentials = new MemoryCredentials();
    const claim = (await example("claim")).replace("1788567300000", "1900000000000");
    const rotatedValue = JSON.parse(claim);
    rotatedValue.access_token = "a".repeat(64);
    const rotated = JSON.stringify(rotatedValue);
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(claim, { status: 200, headers: jsonHeaders }),
      new Response(rotated, { status: 200, headers: jsonHeaders }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });
    const invitation = JSON.parse(await example("invitation"));
    invitation.endpoint = endpoint;
    invitation.expiresAtMs = 1_900_000_000_000;
    invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${invitation.pairingId}&secret=${invitation.secret}&tls_pin=${invitation.tlsPin}`;

    await client.claimPairing(invitation, "Browser device");
    await client.rotateDevice();

    expect(credentials.saves).toHaveLength(2);
    expect(credentials.credential?.accessToken).toBe("a".repeat(64));
    expect(queue.requests[1]?.headers.has("Authorization")).toBe(false);
    expect(await queue.requests[1]?.json()).toEqual({
      secret: "0".repeat(64),
      device_name: "Browser device",
    });
    expect(queue.requests[2]?.headers.get("Authorization")).toBe(`Bearer ${"0".repeat(64)}`);
    expect(queue.requests.every((request: Request) => request.redirect === "error")).toBe(true);
  });

  it("rejects an oversized UTF-8 claim body before any network request", async () => {
    const invitation = JSON.parse(await example("invitation")) as Record<string, unknown>;
    invitation.endpoint = endpoint;
    invitation.expiresAtMs = Date.now() + 60_000;
    invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${String(invitation.pairingId)}&secret=${String(invitation.secret)}&tls_pin=${String(invitation.tlsPin)}`;
    const queue = queuedFetch([]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials: new MemoryCredentials(),
      fetch: queue.fetch,
    });

    await expect(
      client.claimPairing(invitation as never, "😀".repeat(1_024)),
    ).rejects.toMatchObject({
      code: "protocol_validation",
      validator: "HubClaimRequest.size",
    });
    expect(queue.requests).toHaveLength(0);
  });

  it.each([400, 415, 422])(
    "surfaces a nonempty claim extractor response as a body-free HTTP error: %i",
    async (status) => {
      const invitation = JSON.parse(await example("invitation")) as Record<string, unknown>;
      invitation.endpoint = endpoint;
      invitation.expiresAtMs = Date.now() + 60_000;
      invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${String(invitation.pairingId)}&secret=${String(invitation.secret)}&tls_pin=${String(invitation.tlsPin)}`;
      const queue = queuedFetch([
        await discoveryResponse(),
        new Response("private extractor detail", {
          status,
          headers: { "Content-Type": "text/plain; charset=utf-8" },
        }),
      ]);
      const client = createHubClient({
        endpoint,
        expectedHubId: hubId,
        credentials: new MemoryCredentials(),
        fetch: queue.fetch,
      });

      const error = await captureError(client.claimPairing(invitation as never, "Device"));

      expect(error).toMatchObject({ code: "hub_http_error", status });
      expect(error).not.toHaveProperty("body");
      expect(String(error)).not.toContain("private extractor detail");
      expect(queue.requests).toHaveLength(2);
    },
  );

  it.each([400, 415, 422])(
    "rejects an empty claim extractor response as invalid: %i",
    async (status) => {
      const invitation = JSON.parse(await example("invitation")) as Record<string, unknown>;
      invitation.endpoint = endpoint;
      invitation.expiresAtMs = Date.now() + 60_000;
      invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${String(invitation.pairingId)}&secret=${String(invitation.secret)}&tls_pin=${String(invitation.tlsPin)}`;
      const queue = queuedFetch([
        await discoveryResponse(),
        new Response(null, { status, headers: { "Content-Type": "text/plain" } }),
      ]);
      const client = createHubClient({
        endpoint,
        expectedHubId: hubId,
        credentials: new MemoryCredentials(),
        fetch: queue.fetch,
      });

      await expect(client.claimPairing(invitation as never, "Device")).rejects.toMatchObject({
        code: "protocol_validation",
        validator: "HubError.body",
      });
    },
  );

  it.each([400, 415, 422])(
    "rejects a claim extractor response with the wrong media type: %i",
    async (status) => {
      const invitation = JSON.parse(await example("invitation")) as Record<string, unknown>;
      invitation.endpoint = endpoint;
      invitation.expiresAtMs = Date.now() + 60_000;
      invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${String(invitation.pairingId)}&secret=${String(invitation.secret)}&tls_pin=${String(invitation.tlsPin)}`;
      const queue = queuedFetch([
        await discoveryResponse(),
        new Response("private extractor detail", {
          status,
          headers: { "Content-Type": "application/json" },
        }),
      ]);
      const client = createHubClient({
        endpoint,
        expectedHubId: hubId,
        credentials: new MemoryCredentials(),
        fetch: queue.fetch,
      });

      await expect(client.claimPairing(invitation as never, "Device")).rejects.toMatchObject({
        code: "protocol_validation",
        validator: "HubError.contentType",
      });
    },
  );

  it("does not save malformed or expired claim replies", async () => {
    const credentials = new MemoryCredentials();
    const malformed = (await example("claim")).replace(
      '"expires_at_ms":1788567300000',
      '"expires_at_ms":1',
    );
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(malformed, { status: 200, headers: jsonHeaders }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });
    const invitation = JSON.parse(await example("invitation"));
    invitation.endpoint = endpoint;
    invitation.expiresAtMs = Date.now() + 60_000;
    invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${invitation.pairingId}&secret=${invitation.secret}&tls_pin=${invitation.tlsPin}`;

    await expect(client.claimPairing(invitation, "Device")).rejects.toMatchObject({
      code: "protocol_validation",
    });
    expect(credentials.saves).toEqual([]);
  });

  it("does not save a claim that completes after logout", async () => {
    const credentials = new MemoryCredentials();
    const invitation = JSON.parse(await example("invitation")) as Record<string, unknown>;
    invitation.endpoint = endpoint;
    invitation.expiresAtMs = Date.now() + 60_000;
    invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${String(invitation.pairingId)}&secret=${String(invitation.secret)}&tls_pin=${String(invitation.tlsPin)}`;
    const claimStarted = deferred<void>();
    let resolveClaim: ((response: Response) => void) | undefined;
    const requests: Request[] = [];
    const fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      requests.push(new Request(input, init));
      if (requests.length === 1) return discoveryResponse();
      claimStarted.resolve(undefined);
      return new Promise<Response>((resolve) => {
        resolveClaim = resolve;
      });
    };
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch,
    });

    const pending = client.claimPairing(invitation as never, "Late device");
    const pendingError = pending.then(
      () => new Error("claim unexpectedly resolved"),
      (error: unknown) => error,
    );
    await claimStarted.promise;
    await client.logout();
    const claim = (await example("claim")).replace("1788567300000", "1900000000000");
    resolveClaim?.(new Response(claim, { status: 200, headers: jsonHeaders }));

    await expect(pendingError).resolves.toMatchObject({ name: "AbortError" });
    expect(credentials.saves).toEqual([]);
    expect(credentials.credential).toBeUndefined();
    expect(requests).toHaveLength(2);
  });

  it.each(["logout", "dispose"] as const)(
    "does not dispatch an authenticated request after a pending credential load during %s",
    async (action) => {
      const loaded = deferred<HubCredential | undefined>();
      const loadStarted = deferred<void>();
      const credential: HubCredential = {
        accessToken: "a".repeat(64),
        deviceId: hubId,
        expiresAtMs: 1_900_000_000_000,
      };
      const credentials: HubCredentialStore = {
        load: () => {
          loadStarted.resolve(undefined);
          return loaded.promise;
        },
        save: async () => undefined,
        clear: async () => undefined,
      };
      const requests: Request[] = [];
      const fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
        requests.push(new Request(input, init));
        if (requests.length === 1) return discoveryResponse(["query.vehicles", "query.current"]);
        return new Response(await example("vehicles"), { status: 200, headers: jsonHeaders });
      };
      const client = createHubClient({ endpoint, expectedHubId: hubId, credentials, fetch });
      await client.discover();

      const pending = client.vehicles();
      await loadStarted.promise;
      if (action === "logout") await client.logout();
      else client.dispose();
      loaded.resolve(credential);

      await expect(pending).rejects.toMatchObject({ name: "AbortError" });
      expect(requests).toHaveLength(1);
    },
  );

  it.each(["logout", "dispose"] as const)(
    "does not dispatch rotation after a pending credential load during %s",
    async (action) => {
      const loaded = deferred<HubCredential | undefined>();
      const loadStarted = deferred<void>();
      const credential: HubCredential = {
        accessToken: "a".repeat(64),
        deviceId: hubId,
        expiresAtMs: 1_900_000_000_000,
      };
      const credentials: HubCredentialStore = {
        load: () => {
          loadStarted.resolve(undefined);
          return loaded.promise;
        },
        save: async () => undefined,
        clear: async () => undefined,
      };
      const claim = (await example("claim")).replace("1788567300000", "1900000000000");
      const requests: Request[] = [];
      const fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
        requests.push(new Request(input, init));
        if (requests.length === 1) return discoveryResponse();
        return new Response(claim, { status: 200, headers: jsonHeaders });
      };
      const client = createHubClient({ endpoint, expectedHubId: hubId, credentials, fetch });
      await client.discover();

      const pending = client.rotateDevice();
      await loadStarted.promise;
      if (action === "logout") await client.logout();
      else client.dispose();
      loaded.resolve(credential);

      await expect(pending).rejects.toMatchObject({ name: "AbortError" });
      expect(requests).toHaveLength(1);
    },
  );

  it("waits for an in-flight credential save before clearing on logout", async () => {
    const saveRelease = deferred<void>();
    const saveStarted = deferred<void>();
    let clearStarted = false;
    let credential: HubCredential | undefined;
    const credentials: HubCredentialStore = {
      load: () => credential,
      save: async (next) => {
        saveStarted.resolve(undefined);
        await saveRelease.promise;
        credential = next;
      },
      clear: async () => {
        clearStarted = true;
        credential = undefined;
      },
    };
    const invitation = JSON.parse(await example("invitation")) as Record<string, unknown>;
    invitation.endpoint = endpoint;
    invitation.expiresAtMs = Date.now() + 60_000;
    invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${String(invitation.pairingId)}&secret=${String(invitation.secret)}&tls_pin=${String(invitation.tlsPin)}`;
    const claim = (await example("claim")).replace("1788567300000", "1900000000000");
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(claim, { status: 200, headers: jsonHeaders }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });

    const pendingClaim = client.claimPairing(invitation as never, "Queued device");
    const pendingClaimError = pendingClaim.then(
      () => new Error("claim unexpectedly resolved"),
      (error: unknown) => error,
    );
    await saveStarted.promise;
    const pendingLogout = client.logout();
    expect(clearStarted).toBe(false);
    saveRelease.resolve(undefined);

    await pendingLogout;
    await expect(pendingClaimError).resolves.toMatchObject({ name: "AbortError" });
    expect(clearStarted).toBe(true);
    expect(credential).toBeUndefined();
  });

  it("does not report a claim success when dispose interrupts an in-flight save", async () => {
    const saveRelease = deferred<void>();
    const saveStarted = deferred<void>();
    let credential: HubCredential | undefined;
    let clearCalls = 0;
    const credentials: HubCredentialStore = {
      load: () => credential,
      save: async (next) => {
        saveStarted.resolve(undefined);
        await saveRelease.promise;
        credential = next;
      },
      clear: async () => {
        clearCalls += 1;
        credential = undefined;
      },
    };
    const invitation = JSON.parse(await example("invitation")) as Record<string, unknown>;
    invitation.endpoint = endpoint;
    invitation.expiresAtMs = Date.now() + 60_000;
    invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${String(invitation.pairingId)}&secret=${String(invitation.secret)}&tls_pin=${String(invitation.tlsPin)}`;
    const claim = (await example("claim")).replace("1788567300000", "1900000000000");
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(claim, { status: 200, headers: jsonHeaders }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });

    const pendingClaim = client.claimPairing(invitation as never, "Disposed device");
    const pendingClaimError = pendingClaim.then(
      () => new Error("claim unexpectedly resolved"),
      (error: unknown) => error,
    );
    await saveStarted.promise;
    client.dispose();
    saveRelease.resolve(undefined);

    await expect(pendingClaimError).resolves.toMatchObject({ name: "AbortError" });
    expect(clearCalls).toBe(0);
    expect(credential).toBeDefined();
  });

  it("surfaces a credential save failure without replaying the claim", async () => {
    const saveError = new Error("save failed");
    let saveCalls = 0;
    const credentials: HubCredentialStore = {
      load: async () => undefined,
      save: async () => {
        saveCalls += 1;
        throw saveError;
      },
      clear: async () => undefined,
    };
    const invitation = JSON.parse(await example("invitation")) as Record<string, unknown>;
    invitation.endpoint = endpoint;
    invitation.expiresAtMs = Date.now() + 60_000;
    invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${String(invitation.pairingId)}&secret=${String(invitation.secret)}&tls_pin=${String(invitation.tlsPin)}`;
    const claim = (await example("claim")).replace("1788567300000", "1900000000000");
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(claim, { status: 200, headers: jsonHeaders }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });

    await expect(client.claimPairing(invitation as never, "Failed save device")).rejects.toBe(
      saveError,
    );
    expect(saveCalls).toBe(1);
    expect(queue.requests).toHaveLength(2);
  });

  it("surfaces a credential clear failure without replaying the clear", async () => {
    const clearError = new Error("clear failed");
    let clearCalls = 0;
    const credentials: HubCredentialStore = {
      load: async () => undefined,
      save: async () => undefined,
      clear: async () => {
        clearCalls += 1;
        throw clearError;
      },
    };
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queuedFetch([]).fetch,
    });

    await expect(client.logout()).rejects.toBe(clearError);
    expect(clearCalls).toBe(1);
  });

  it("orders a credential save started during logout after the clear", async () => {
    const clearRelease = deferred<void>();
    const clearStarted = deferred<void>();
    let saveStarted = false;
    let credential: HubCredential | undefined;
    const credentials: HubCredentialStore = {
      load: () => credential,
      save: async (next) => {
        saveStarted = true;
        credential = next;
      },
      clear: async () => {
        clearStarted.resolve(undefined);
        await clearRelease.promise;
        credential = undefined;
      },
    };
    const invitation = JSON.parse(await example("invitation")) as Record<string, unknown>;
    invitation.endpoint = endpoint;
    invitation.expiresAtMs = Date.now() + 60_000;
    invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${String(invitation.pairingId)}&secret=${String(invitation.secret)}&tls_pin=${String(invitation.tlsPin)}`;
    const claim = (await example("claim")).replace("1788567300000", "1900000000000");
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(claim, { status: 200, headers: jsonHeaders }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });

    const pendingLogout = client.logout();
    await clearStarted.promise;
    const pendingClaim = client.claimPairing(invitation as never, "Post-logout device");
    expect(saveStarted).toBe(false);
    clearRelease.resolve(undefined);

    await pendingLogout;
    await pendingClaim;
    expect(saveStarted).toBe(true);
    expect(credential).toBeDefined();
  });

  it("does not dispatch a claim when cached discovery becomes stale before its request", async () => {
    const credentials = new MemoryCredentials();
    const claim = (await example("claim")).replace("1788567300000", "1900000000000");
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(claim, { status: 200, headers: jsonHeaders }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });
    await client.discover();

    const invitation = JSON.parse(await example("invitation")) as Record<string, unknown>;
    invitation.endpoint = endpoint;
    invitation.expiresAtMs = Date.now() + 60_000;
    invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${String(invitation.pairingId)}&secret=${String(invitation.secret)}&tls_pin=${String(invitation.tlsPin)}`;
    const pendingClaim = client.claimPairing(invitation as never, "Stale discovery device");
    await client.logout();

    await expect(pendingClaim).rejects.toMatchObject({ name: "AbortError" });
    expect(queue.requests).toHaveLength(1);
  });

  it.each(["logout", "dispose"] as const)(
    "cancels a response body that is waiting for another chunk during %s",
    async (action) => {
      const secondPullStarted = deferred<void>();
      const releaseSecondPull = deferred<void>();
      const cancelled = deferred<void>();
      let pulls = 0;
      const stream = new ReadableStream<Uint8Array>({
        pull(controller) {
          pulls += 1;
          if (pulls === 1) {
            controller.enqueue(new TextEncoder().encode("{"));
            return;
          }
          if (pulls === 2) {
            secondPullStarted.resolve(undefined);
            return releaseSecondPull.promise.then(() => {
              controller.enqueue(new TextEncoder().encode("}"));
            });
          }
          return undefined;
        },
        cancel() {
          cancelled.resolve(undefined);
        },
      });
      const credentials = new MemoryCredentials();
      credentials.credential = {
        accessToken: "a".repeat(64),
        deviceId: hubId,
        expiresAtMs: 1_900_000_000_000,
      };
      const requests: Request[] = [];
      const fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
        requests.push(new Request(input, init));
        if (requests.length === 1) return discoveryResponse(["query.vehicles", "query.current"]);
        return new Response(stream, { status: 200, headers: jsonHeaders });
      };
      const client = createHubClient({ endpoint, expectedHubId: hubId, credentials, fetch });
      await client.discover();

      const pending = client.current(hubId);
      await secondPullStarted.promise;
      if (action === "logout") await client.logout();
      else client.dispose();
      releaseSecondPull.resolve(undefined);

      await expect(pending).rejects.toMatchObject({ name: "AbortError" });
      await expect(cancelled.promise).resolves.toBeUndefined();
      expect(requests).toHaveLength(2);
    },
  );

  it("logout clears credentials and aborts pending reads; dispose aborts later calls", async () => {
    const credentials = new MemoryCredentials();
    const requests: Request[] = [];
    let pendingSignal: AbortSignal | undefined;
    const fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const request = new Request(input, init);
      requests.push(request);
      if (requests.length === 1) return discoveryResponse();
      pendingSignal = request.signal;
      return new Promise<Response>(
        (_resolve: (value: Response) => void, reject: (reason: unknown) => void) => {
          request.signal.addEventListener("abort", () => reject(request.signal.reason));
        },
      );
    };
    const client = createHubClient({ endpoint, expectedHubId: hubId, credentials, fetch });
    await client.discover();
    const pending = client.vehicles();
    await new Promise<void>((resolve: () => void) => setTimeout(resolve, 0));

    await client.logout();

    await expect(pending).rejects.toBeDefined();
    expect(pendingSignal?.aborted).toBe(true);
    expect(credentials.clearCount).toBe(1);
    client.dispose();
    await expect(client.health()).rejects.toMatchObject({ code: "client_disposed" });
  });

  it("does not let late discovery repopulate identity state after logout", async () => {
    const credentials = new MemoryCredentials();
    let resolveOld: ((response: Response) => void) | undefined;
    const requests: Request[] = [];
    const fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const request = new Request(input, init);
      requests.push(request);
      if (requests.length === 1) {
        return new Promise<Response>((resolve) => {
          resolveOld = resolve;
        });
      }
      if (requests.length === 2) return discoveryResponse();
      return new Response(await example("vehicles"), { status: 200, headers: jsonHeaders });
    };
    const client = createHubClient({ endpoint, expectedHubId: hubId, credentials, fetch });
    const stale = client.discover();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));

    await client.logout();
    resolveOld?.(await discoveryResponse());
    await expect(stale).rejects.toMatchObject({ name: "AbortError" });
    await client.vehicles();

    expect(requests.map((request) => new URL(request.url).pathname)).toEqual([
      "/.well-known/teslatlas-hub",
      "/.well-known/teslatlas-hub",
      "/v1/vehicles",
    ]);
    expect(requests[0]?.signal.aborted).toBe(true);
    expect(requests[1]?.headers.has("Authorization")).toBe(false);
  });

  it("does not reuse cached identity after explicit discovery failure", async () => {
    const credentials = new MemoryCredentials();
    credentials.credential = {
      accessToken: "a".repeat(64),
      deviceId: hubId,
      expiresAtMs: 1_900_000_000_000,
    };
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(JSON.stringify({ error: { code: "service_unavailable", message: "down" } }), {
        status: 503,
        headers: jsonHeaders,
      }),
      await discoveryResponse(),
      new Response(await example("vehicles"), { status: 200, headers: jsonHeaders }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });

    await client.discover();
    await expect(client.discover()).rejects.toMatchObject({
      code: "service_unavailable",
      status: 503,
    });
    await expect(client.vehicles()).resolves.toMatchObject({
      value: { vehicles: [{ vehicleId: hubId }] },
    });

    expect(queue.requests.map((request) => new URL(request.url).pathname)).toEqual([
      "/.well-known/teslatlas-hub",
      "/.well-known/teslatlas-hub",
      "/.well-known/teslatlas-hub",
      "/v1/vehicles",
    ]);
    expect(queue.requests[1]?.headers.has("Authorization")).toBe(false);
    expect(queue.requests[2]?.headers.has("Authorization")).toBe(false);
    expect(queue.requests[3]?.headers.get("Authorization")).toBe(`Bearer ${"a".repeat(64)}`);
  });

  it("cancels a streamed response as soon as it crosses the 1 MiB body limit", async () => {
    const cancelled = deferred<void>();
    let pulls = 0;
    const stream = new ReadableStream<Uint8Array>(
      {
        pull(controller) {
          pulls += 1;
          if (pulls === 1) {
            controller.enqueue(new Uint8Array(1_048_576));
            return;
          }
          if (pulls === 2) {
            controller.enqueue(new Uint8Array(1));
            return;
          }
          controller.close();
        },
        cancel() {
          cancelled.resolve(undefined);
        },
      },
      { highWaterMark: 0 },
    );
    const credentials = new MemoryCredentials();
    credentials.credential = {
      accessToken: "a".repeat(64),
      deviceId: hubId,
      expiresAtMs: 1_900_000_000_000,
    };
    const queue = queuedFetch([
      await discoveryResponse(),
      new Response(stream, {
        status: 200,
        headers: { ...jsonHeaders, ETag: '"drive-page"', "Cache-Control": "no-store" },
      }),
    ]);
    const client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
      fetch: queue.fetch,
    });

    await expect(client.drives(hubId)).rejects.toMatchObject({
      code: "protocol_validation",
      validator: "HubDrives.size",
    });
    await expect(cancelled.promise).resolves.toBeUndefined();
    expect(pulls).toBe(2);
  });
});
