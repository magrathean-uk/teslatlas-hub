import { readFile } from "node:fs/promises";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createHubClient, hubLogoutRevokeDeadlineMs } from "../../src/hub/client.js";
import type { HubClaim, HubCredential, HubCredentialStore } from "../../src/hub/models.js";

const hubId = "11111111-1111-4111-8111-111111111111";
const otherHubId = "22222222-2222-4222-8222-222222222222";
const endpoint = "https://hub.example.invalid";
const jsonHeaders = { "Content-Type": "application/json", "X-Request-ID": "request-1" };
const profileRoot = new URL("../../../protocol/profiles/hub-http-v1/1.2.0/", import.meta.url);
const discoveryPath = "/.well-known/teslatlas-hub";
const revokePath = "/v1/device/revoke";
const rotatePath = "/v1/device/rotate";
const expiresAtMs = 1_900_000_000_000;
const token = "a".repeat(64);
const successorToken = "b".repeat(64);
const stored: HubCredential = { accessToken: token, deviceId: hubId, expiresAtMs };

async function example(name: string): Promise<string> {
  return readFile(new URL(`examples/${name}.json`, profileRoot), "utf8");
}

// Read once so that no handler below waits on file I/O: the interleavings stay deterministic.
const [discoveryText, vehiclesText, claimText, maxLifetimeText, invitationText] = await Promise.all(
  [
    example("discovery"),
    example("vehicles"),
    example("claim"),
    example("rotate-max-lifetime"),
    example("invitation"),
  ],
);

type Handler = (request: Request) => Response | Promise<Response>;

interface Routes {
  readonly events?: string[];
  readonly discovery?: Handler;
  readonly revoke?: Handler;
  readonly rotate?: Handler;
  readonly vehicles?: Handler;
  readonly claim?: Handler;
}

function discoveryWith(capabilities: readonly string[]): Response {
  const value = JSON.parse(discoveryText) as { capabilities: string[] };
  value.capabilities = [...capabilities];
  return new Response(JSON.stringify(value), { status: 200, headers: jsonHeaders });
}

function defaultDiscovery(): Response {
  return new Response(discoveryText, { status: 200, headers: jsonHeaders });
}

function noRevoke(): Response {
  return discoveryWith(["query.vehicles", "query.current"]);
}

/** A fetch that answers by path and keeps every request, with whether its signal was already aborted. */
function stubHub(routes: Routes = {}) {
  const calls: Request[] = [];
  const abortedAtDispatch: boolean[] = [];
  const fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const request = new Request(input, init);
    calls.push(request);
    abortedAtDispatch.push(init?.signal?.aborted === true);
    const { pathname } = new URL(request.url);
    if (pathname === discoveryPath) return (routes.discovery ?? defaultDiscovery)(request);
    if (pathname === revokePath) {
      routes.events?.push("revoke");
      return (routes.revoke ?? (() => new Response(null, { status: 204 })))(request);
    }
    if (pathname === rotatePath) {
      if (routes.rotate === undefined) throw new Error("unexpected rotate request");
      return routes.rotate(request);
    }
    if (pathname === "/v1/vehicles") {
      return (
        routes.vehicles ?? (() => new Response(vehiclesText, { status: 200, headers: jsonHeaders }))
      )(request);
    }
    if (pathname.startsWith("/v1/pairings/") && routes.claim !== undefined) {
      return routes.claim(request);
    }
    throw new Error(`unexpected request ${pathname}`);
  };
  return {
    fetch,
    calls,
    abortedAtDispatch,
    paths: () => calls.map((request) => new URL(request.url).pathname),
    to: (path: string) => calls.filter((request) => new URL(request.url).pathname === path),
  };
}

class RecordingStore implements HubCredentialStore {
  credential: HubCredential | undefined;
  readonly saves: HubCredential[] = [];
  readonly events: string[];
  loads = 0;
  clears = 0;
  loadError: unknown;
  clearError: unknown;
  onLoad: ((call: number) => Promise<void> | void) | undefined;
  onSave: (() => Promise<void> | void) | undefined;
  onClear: ((call: number) => Promise<void> | void) | undefined;

  constructor(credential: HubCredential | undefined, events: string[] = []) {
    this.credential = credential;
    this.events = events;
  }

  async load(): Promise<HubCredential | undefined> {
    this.loads += 1;
    // A slow store answers with what was there when the read began.
    const snapshot = this.credential;
    await this.onLoad?.(this.loads);
    if (this.loadError !== undefined) throw this.loadError;
    return snapshot;
  }

  async save(credential: HubCredential): Promise<void> {
    this.events.push("save");
    await this.onSave?.();
    this.saves.push(credential);
    this.credential = credential;
  }

  async clear(): Promise<void> {
    this.clears += 1;
    this.events.push("clear");
    await this.onClear?.(this.clears);
    if (this.clearError !== undefined) throw this.clearError;
    this.credential = undefined;
  }
}

function clientFor(
  store: HubCredentialStore,
  hub: { fetch: (input: RequestInfo | URL, init?: RequestInit) => Promise<Response> },
) {
  return createHubClient({
    endpoint,
    expectedHubId: hubId,
    credentials: store,
    fetch: hub.fetch,
    claimTransport: ({ url, body, signal }) =>
      hub.fetch(url, {
        method: "POST",
        redirect: "error",
        signal,
        body,
        headers: { "Content-Type": "application/json" },
      }),
  });
}

function deferred<T = void>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function outcome(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
    return undefined;
  } catch (error) {
    return error;
  }
}

/** Let every ready microtask and I/O-free callback run; under fake timers advance by nothing. */
async function settle(): Promise<void> {
  if (vi.isFakeTimers()) await vi.advanceTimersByTimeAsync(0);
  else await new Promise<void>((resolve) => setImmediate(resolve));
}

/** A response that only an abort ends, like a real fetch whose connection stalls. */
function stalls(request: Request): Promise<Response> {
  return new Promise<Response>((_resolve, reject) => {
    request.signal.addEventListener("abort", () => reject(request.signal.reason), { once: true });
  });
}

function rotateAnswer(overrides: Record<string, unknown> = {}): Response {
  const value = {
    ...(JSON.parse(claimText) as Record<string, unknown>),
    access_token: successorToken,
    expires_at_ms: expiresAtMs + 1_000,
    ...overrides,
  };
  return new Response(JSON.stringify(value), { status: 200, headers: jsonHeaders });
}

function maxLifetimeAnswer(overrides: Record<string, unknown> = {}): Response {
  const value = {
    ...(JSON.parse(maxLifetimeText) as Record<string, unknown>),
    access_token: token,
    device_id: hubId,
    expires_at_ms: expiresAtMs,
    ...overrides,
  };
  return new Response(JSON.stringify(value), { status: 200, headers: jsonHeaders });
}

function claimAnswer(): Response {
  return new Response(claimText.replace("1788567300000", String(expiresAtMs)), {
    status: 200,
    headers: jsonHeaders,
  });
}

function pairingInvitation(): never {
  const invitation = JSON.parse(invitationText) as Record<string, unknown>;
  invitation.endpoint = endpoint;
  invitation.expiresAtMs = Date.now() + 60_000;
  invitation.pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${String(invitation.pairingId)}&secret=${String(invitation.secret)}&tls_pin=${String(invitation.tlsPin)}`;
  return invitation as never;
}

afterEach(() => {
  vi.useRealTimers();
});

describe("logout revokes the device at the Hub, best effort", () => {
  it.each([204, 401])(
    "sends one body-free revoke with the stored bearer before it clears; %i counts as revoked",
    async (status) => {
      const events: string[] = [];
      const store = new RecordingStore(stored, events);
      const hub = stubHub({ events, revoke: () => new Response(null, { status }) });
      const client = clientFor(store, hub);
      await client.discover();

      await expect(client.logout()).resolves.toBeUndefined();

      expect(hub.paths()).toEqual([discoveryPath, revokePath]);
      const revoke = hub.to(revokePath)[0];
      expect(revoke?.method).toBe("POST");
      expect(revoke?.headers.get("Authorization")).toBe(`Bearer ${token}`);
      expect(revoke?.headers.has("Content-Type")).toBe(false);
      expect(revoke?.body).toBeNull();
      expect(revoke?.redirect).toBe("error");
      // The session signal logout aborted is not the one the revoke travels on.
      expect(hub.abortedAtDispatch[1]).toBe(false);
      expect(events).toEqual(["revoke", "clear"]);
      expect(store.credential).toBeUndefined();
      expect(store.clears).toBe(1);
    },
  );

  it.each<[number, string | null]>([
    [400, "private detail"],
    [404, null],
    [500, null],
    [503, null],
  ])(
    "treats %i as not revoked: one request, no retry, the clear still happens",
    async (status, body) => {
      const events: string[] = [];
      const store = new RecordingStore(stored, events);
      const hub = stubHub({ events, revoke: () => new Response(body, { status }) });
      const client = clientFor(store, hub);
      await client.discover();

      await expect(client.logout()).resolves.toBeUndefined();
      await settle();

      expect(hub.to(revokePath)).toHaveLength(1);
      expect(hub.paths()).toEqual([discoveryPath, revokePath]);
      expect(events).toEqual(["revoke", "clear"]);
      expect(store.credential).toBeUndefined();
    },
  );

  it("treats a transport error as not revoked without leaking the bearer", async () => {
    const events: string[] = [];
    const store = new RecordingStore(stored, events);
    const hub = stubHub({
      events,
      revoke: () => {
        throw new Error(`socket closed after Bearer ${token}`);
      },
    });
    const client = clientFor(store, hub);
    await client.discover();

    await expect(client.logout()).resolves.toBeUndefined();
    await settle();

    expect(hub.to(revokePath)).toHaveLength(1);
    expect(events).toEqual(["revoke", "clear"]);
    expect(store.credential).toBeUndefined();
  });

  it("revokes once and still rejects with the store's clear failure", async () => {
    const clearError = new Error("clear failed");
    const events: string[] = [];
    const store = new RecordingStore(stored, events);
    store.clearError = clearError;
    const hub = stubHub({ events });
    const client = clientFor(store, hub);
    await client.discover();

    await expect(client.logout()).rejects.toBe(clearError);

    expect(hub.to(revokePath)).toHaveLength(1);
    expect(store.clears).toBe(1);
  });

  it("sends nothing and loads nothing when the cached discovery lacks auth.revoke", async () => {
    const events: string[] = [];
    const store = new RecordingStore(stored, events);
    const hub = stubHub({ events, discovery: noRevoke });
    const client = clientFor(store, hub);
    await client.discover();

    await client.logout();

    expect(hub.paths()).toEqual([discoveryPath]);
    expect(store.loads).toBe(0);
    expect(events).toEqual(["clear"]);
    expect(store.credential).toBeUndefined();
  });

  it("sends nothing when nothing is stored, and fetches no discovery for it", async () => {
    const cached = new RecordingStore(undefined);
    const cachedHub = stubHub();
    const cachedClient = clientFor(cached, cachedHub);
    await cachedClient.discover();
    await cachedClient.logout();
    expect(cachedHub.paths()).toEqual([discoveryPath]);
    expect(cached.clears).toBe(1);

    const fresh = new RecordingStore(undefined);
    const freshHub = stubHub();
    await clientFor(fresh, freshHub).logout();
    expect(freshHub.paths()).toEqual([]);
    expect(fresh.clears).toBe(1);
  });

  it.each<[string, HubCredential]>([
    ["a malformed token", { ...stored, accessToken: "short" }],
    ["an expired credential", { ...stored, expiresAtMs: Date.now() - 1_000 }],
  ])("does not send %s as a bearer", async (_label, credential) => {
    const store = new RecordingStore(credential);
    const hub = stubHub();
    const client = clientFor(store, hub);
    await client.discover();

    await client.logout();

    expect(hub.paths()).toEqual([discoveryPath]);
    expect(store.credential).toBeUndefined();
  });

  it("clears even when loading the credential for the revoke fails", async () => {
    const store = new RecordingStore(stored);
    store.loadError = new Error(`store unreadable for ${token}`);
    const hub = stubHub();
    const client = clientFor(store, hub);
    await client.discover();

    await expect(client.logout()).resolves.toBeUndefined();

    expect(hub.paths()).toEqual([discoveryPath]);
    expect(store.clears).toBe(1);
  });

  it("makes one discovery fetch when none is cached and revokes at the Hub it names", async () => {
    const events: string[] = [];
    const store = new RecordingStore(stored, events);
    const hub = stubHub({ events });
    const client = clientFor(store, hub);

    await client.logout();

    expect(hub.paths()).toEqual([discoveryPath, revokePath]);
    expect(hub.to(discoveryPath)[0]?.headers.has("Authorization")).toBe(false);
    expect(hub.to(revokePath)[0]?.headers.get("Authorization")).toBe(`Bearer ${token}`);
    expect(events).toEqual(["revoke", "clear"]);
    expect(store.credential).toBeUndefined();
  });

  it.each<[string, Handler]>([
    [
      "a different Hub identity",
      () =>
        new Response(discoveryText.replace(hubId, otherHubId), {
          status: 200,
          headers: jsonHeaders,
        }),
    ],
    ["no auth.revoke", noRevoke],
    [
      "a 503",
      () =>
        new Response(JSON.stringify({ error: { code: "service_unavailable", message: "down" } }), {
          status: 503,
          headers: jsonHeaders,
        }),
    ],
  ])("never sends the bearer when the fresh discovery shows %s", async (_label, discovery) => {
    const store = new RecordingStore(stored);
    const hub = stubHub({ discovery });
    const client = clientFor(store, hub);

    await expect(client.logout()).resolves.toBeUndefined();

    expect(hub.paths()).toEqual([discoveryPath]);
    expect(hub.calls.every((request) => !request.headers.has("Authorization"))).toBe(true);
    expect(store.credential).toBeUndefined();
  });

  it("revokes the credential a claim save in flight is writing, after that save", async () => {
    const events: string[] = [];
    const saveGate = deferred();
    const saveStarted = deferred();
    const store = new RecordingStore(undefined, events);
    store.onSave = async () => {
      saveStarted.resolve();
      await saveGate.promise;
    };
    const hub = stubHub({
      events,
      claim: claimAnswer,
    });
    const client = clientFor(store, hub);

    const claim = outcome(client.claimPairing(pairingInvitation(), "Device"));
    await saveStarted.promise;
    const loggingOut = client.logout();
    saveGate.resolve();
    await loggingOut;

    expect(await claim).toMatchObject({ name: "AbortError" });
    expect(hub.to(revokePath)).toHaveLength(1);
    expect(hub.to(revokePath)[0]?.headers.get("Authorization")).toBe(`Bearer ${"0".repeat(64)}`);
    expect(events).toEqual(["save", "revoke", "clear"]);
    expect(store.credential).toBeUndefined();
  });

  it("abandons a pending revoke on dispose() and still clears", async () => {
    const events: string[] = [];
    const store = new RecordingStore(stored, events);
    const revokeStarted = deferred();
    const hub = stubHub({
      events,
      revoke: (request) => {
        revokeStarted.resolve();
        return stalls(request);
      },
    });
    const client = clientFor(store, hub);
    await client.discover();

    const loggingOut = client.logout();
    await revokeStarted.promise;
    client.dispose();

    await expect(loggingOut).resolves.toBeUndefined();
    expect(hub.to(revokePath)).toHaveLength(1);
    expect(hub.to(revokePath)[0]?.signal.aborted).toBe(true);
    expect(events).toEqual(["revoke", "clear"]);
    expect(store.credential).toBeUndefined();
  });

  it("loads and sends nothing when dispose() comes before the queued revoke starts, yet clears", async () => {
    const events: string[] = [];
    const saveGate = deferred();
    const saveStarted = deferred();
    const store = new RecordingStore(undefined, events);
    store.onSave = async () => {
      saveStarted.resolve();
      await saveGate.promise;
    };
    const hub = stubHub({
      events,
      claim: claimAnswer,
    });
    const client = clientFor(store, hub);
    const claim = outcome(client.claimPairing(pairingInvitation(), "Device"));
    await saveStarted.promise;
    // The claim's save is in the queue, so logout's entry (revoke, then clear) has not started.
    const loggingOut = client.logout();
    client.dispose();
    saveGate.resolve();

    await expect(loggingOut).resolves.toBeUndefined();
    expect(await claim).toMatchObject({ name: "AbortError" });
    expect(hub.to(revokePath)).toHaveLength(0);
    expect(store.loads).toBe(0);
    expect(events).toEqual(["save", "clear"]);
    expect(store.credential).toBeUndefined();
  });
});

describe("the revoke deadline", () => {
  it("is short enough for the 5 s test timeout", () => {
    expect(hubLogoutRevokeDeadlineMs).toBeLessThanOrEqual(2_000);
    expect(hubLogoutRevokeDeadlineMs).toBeGreaterThan(0);
  });

  it("clears its timer once the revoke is over, so nothing keeps the process waiting", async () => {
    vi.useFakeTimers();
    const store = new RecordingStore(stored);
    const hub = stubHub();
    const client = clientFor(store, hub);
    await client.discover();

    await client.logout();

    expect(hub.to(revokePath)).toHaveLength(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("sends nothing for a credential load that resolves after the deadline", async () => {
    vi.useFakeTimers();
    const lateLoad = deferred();
    const events: string[] = [];
    const store = new RecordingStore(stored, events);
    store.onLoad = () => lateLoad.promise;
    const hub = stubHub({ events });
    const client = clientFor(store, hub);
    await client.discover();

    const loggingOut = client.logout();
    await settle();
    expect(store.loads).toBe(1);
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(hubLogoutRevokeDeadlineMs);
    await expect(loggingOut).resolves.toBeUndefined();
    lateLoad.resolve();
    await settle();

    expect(hub.paths()).toEqual([discoveryPath]);
    expect(events).toEqual(["clear"]);
    expect(store.credential).toBeUndefined();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("stops waiting for a revoke request that never answers, and sends no second one", async () => {
    vi.useFakeTimers();
    const events: string[] = [];
    const store = new RecordingStore(stored, events);
    const hub = stubHub({ events, revoke: stalls });
    const client = clientFor(store, hub);
    await client.discover();

    const loggingOut = client.logout();
    await settle();
    expect(hub.to(revokePath)).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(hubLogoutRevokeDeadlineMs);

    await expect(loggingOut).resolves.toBeUndefined();
    await settle();
    expect(hub.to(revokePath)).toHaveLength(1);
    expect(hub.to(revokePath)[0]?.signal.aborted).toBe(true);
    expect(events).toEqual(["revoke", "clear"]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("is not held past the deadline by a fetch that ignores its signal", async () => {
    vi.useFakeTimers();
    const store = new RecordingStore(stored);
    const hub = stubHub({ revoke: () => new Promise<Response>(() => undefined) });
    const client = clientFor(store, hub);
    await client.discover();

    const loggingOut = client.logout();
    await settle();
    await vi.advanceTimersByTimeAsync(hubLogoutRevokeDeadlineMs);

    await expect(loggingOut).resolves.toBeUndefined();
    expect(store.credential).toBeUndefined();
  });

  it("bounds the discovery fetch that revoke makes when none is cached", async () => {
    vi.useFakeTimers();
    const store = new RecordingStore(stored);
    const hub = stubHub({ discovery: stalls });
    const client = clientFor(store, hub);

    const loggingOut = client.logout();
    await settle();
    expect(hub.paths()).toEqual([discoveryPath]);
    await vi.advanceTimersByTimeAsync(hubLogoutRevokeDeadlineMs);

    await expect(loggingOut).resolves.toBeUndefined();
    expect(hub.paths()).toEqual([discoveryPath]);
    expect(store.credential).toBeUndefined();
  });
});

describe("the logout barrier", () => {
  it("makes a rotation that starts after logout began wait for the clear instead of rotating the credential about to go", async () => {
    const events: string[] = [];
    const clearGate = deferred();
    const clearStarted = deferred();
    const store = new RecordingStore(stored, events);
    store.onClear = async () => {
      clearStarted.resolve();
      await clearGate.promise;
    };
    const hub = stubHub({ events, rotate: () => rotateAnswer() });
    const client = clientFor(store, hub);
    await client.discover();

    const loggingOut = client.logout();
    await clearStarted.promise;
    // The revoke is done and the clear is pending: the store still holds the old credential.
    expect(store.credential).toEqual(stored);
    const loadsBefore = store.loads;
    const rotation = outcome(client.rotateDevice());
    await settle();
    expect(store.loads).toBe(loadsBefore);
    clearGate.resolve();
    await loggingOut;

    expect(await rotation).toMatchObject({ code: "hub_http_error", status: 401 });
    expect(hub.to(rotatePath)).toHaveLength(0);
    expect(store.saves).toEqual([]);
    expect(store.credential).toBeUndefined();
  });

  it("sends a read that starts after logout began without the credential that was cleared", async () => {
    const clearGate = deferred();
    const clearStarted = deferred();
    const store = new RecordingStore(stored);
    store.onClear = async () => {
      clearStarted.resolve();
      await clearGate.promise;
    };
    const hub = stubHub();
    const client = clientFor(store, hub);
    await client.discover();

    const loggingOut = client.logout();
    await clearStarted.promise;
    const read = client.vehicles();
    await settle();
    clearGate.resolve();
    await loggingOut;

    await expect(read).resolves.toMatchObject({ value: { vehicles: [{ vehicleId: hubId }] } });
    const vehicles = hub.to("/v1/vehicles");
    expect(vehicles).toHaveLength(1);
    expect(vehicles[0]?.headers.has("Authorization")).toBe(false);
  });

  it("settles after a failed clear, so a later read neither hangs nor misses the stored credential", async () => {
    const clearError = new Error("clear failed");
    const store = new RecordingStore(stored);
    store.clearError = clearError;
    const hub = stubHub();
    const client = clientFor(store, hub);
    await client.discover();

    await expect(client.logout()).rejects.toBe(clearError);
    await expect(client.vehicles()).resolves.toMatchObject({
      value: { vehicles: [{ vehicleId: hubId }] },
    });

    expect(hub.to("/v1/vehicles")[0]?.headers.get("Authorization")).toBe(`Bearer ${token}`);
    expect(store.credential).toEqual(stored);
  });

  it("keeps a read waiting through two overlapping logouts until the later clear is done", async () => {
    const gates = [deferred(), deferred()];
    const started = [deferred(), deferred()];
    const store = new RecordingStore(stored);
    store.onClear = async (call) => {
      started[call - 1]?.resolve();
      await gates[call - 1]?.promise;
    };
    const hub = stubHub();
    const client = clientFor(store, hub);
    await client.discover();

    const first = client.logout();
    await started[0]?.promise;
    const second = client.logout();
    const read = client.vehicles();
    await settle();
    const loadsWhileFirstClears = store.loads;
    gates[0]?.resolve();
    await started[1]?.promise;
    await settle();
    // The first clear is done, the second is not: the read still has not loaded.
    expect(store.loads).toBe(loadsWhileFirstClears + 1);
    gates[1]?.resolve();

    await Promise.all([first, second]);
    await read;
    expect(store.loads).toBe(loadsWhileFirstClears + 2);
    expect(hub.to("/v1/vehicles")[0]?.headers.has("Authorization")).toBe(false);
    expect(store.credential).toBeUndefined();
    expect(store.clears).toBe(2);
  });

  it("keeps a read that starts after the first logout finished waiting for the second clear", async () => {
    const gates = [deferred(), deferred()];
    const started = [deferred(), deferred()];
    const store = new RecordingStore(stored);
    store.onClear = async (call) => {
      started[call - 1]?.resolve();
      await gates[call - 1]?.promise;
    };
    const hub = stubHub();
    const client = clientFor(store, hub);
    await client.discover();

    const first = client.logout();
    await started[0]?.promise;
    const second = client.logout();
    gates[0]?.resolve();
    await first;
    await started[1]?.promise;
    // The first logout is over and has released its own barrier; the second clear is still
    // running, so a read that begins now must still wait for it.
    const loadsBeforeRead = store.loads;
    const read = client.vehicles();
    await settle();
    expect(store.loads).toBe(loadsBeforeRead);
    gates[1]?.resolve();

    await second;
    await read;
    expect(store.loads).toBe(loadsBeforeRead + 1);
    expect(hub.to("/v1/vehicles")[0]?.headers.has("Authorization")).toBe(false);
    expect(store.credential).toBeUndefined();
    expect(store.clears).toBe(2);
  });

  it("releases a read that waits on the barrier when the client is disposed, and still clears", async () => {
    const clearGate = deferred();
    const clearStarted = deferred();
    const store = new RecordingStore(stored);
    store.onClear = async () => {
      clearStarted.resolve();
      await clearGate.promise;
    };
    const hub = stubHub();
    const client = clientFor(store, hub);
    await client.discover();

    const loggingOut = client.logout();
    await clearStarted.promise;
    const read = outcome(client.vehicles());
    await settle();
    client.dispose();

    expect(await read).toMatchObject({ name: "AbortError" });
    clearGate.resolve();
    await expect(loggingOut).resolves.toBeUndefined();
    expect(hub.to("/v1/vehicles")).toHaveLength(0);
    expect(store.credential).toBeUndefined();
  });
});

describe("a rotation after an earlier logout", () => {
  it("still saves its successor: the load ran under the latest barrier", async () => {
    const store = new RecordingStore(undefined);
    const hub = stubHub({ claim: claimAnswer, rotate: () => rotateAnswer() });
    const client = clientFor(store, hub);

    await client.logout();
    await client.claimPairing(pairingInvitation(), "Device");
    await client.rotateDevice();

    expect(store.saves.map((credential) => credential.accessToken)).toEqual([
      "0".repeat(64),
      successorToken,
    ]);
    expect(hub.to(rotatePath)[0]?.headers.get("Authorization")).toBe(`Bearer ${"0".repeat(64)}`);
    expect(store.credential?.accessToken).toBe(successorToken);
  });
});

describe("logout overlapping rotateDevice", () => {
  it("leaves the store empty and sends no rotate when logout starts during the rotation's load", async () => {
    const events: string[] = [];
    const firstLoad = deferred();
    const firstLoadStarted = deferred();
    const store = new RecordingStore(stored, events);
    store.onLoad = async (call) => {
      if (call !== 1) return;
      firstLoadStarted.resolve();
      await firstLoad.promise;
    };
    const hub = stubHub({ events, rotate: () => rotateAnswer() });
    const client = clientFor(store, hub);
    await client.discover();

    const rotation = outcome(client.rotateDevice());
    await firstLoadStarted.promise;
    await expect(client.logout()).resolves.toBeUndefined();
    firstLoad.resolve();

    expect(await rotation).toMatchObject({ name: "AbortError" });
    expect(hub.to(rotatePath)).toHaveLength(0);
    expect(hub.to(revokePath)).toHaveLength(1);
    expect(hub.to(revokePath)[0]?.headers.get("Authorization")).toBe(`Bearer ${token}`);
    expect(store.saves).toEqual([]);
    expect(store.credential).toBeUndefined();
    expect(events).toEqual(["revoke", "clear"]);
  });

  it("does not save a rotation whose answer arrives after logout cleared the store", async () => {
    const answer = deferred<Response>();
    const rotateStarted = deferred();
    const store = new RecordingStore(stored);
    const hub = stubHub({
      rotate: () => {
        rotateStarted.resolve();
        return answer.promise;
      },
    });
    const client = clientFor(store, hub);
    await client.discover();

    const rotation = outcome(client.rotateDevice());
    await rotateStarted.promise;
    await client.logout();
    answer.resolve(rotateAnswer());

    expect(await rotation).toMatchObject({ name: "AbortError" });
    expect(hub.to(rotatePath)).toHaveLength(1);
    expect(hub.to(rotatePath)[0]?.headers.get("Authorization")).toBe(`Bearer ${token}`);
    // The revoke that logout sent used the same bearer; nothing the rotation answered was kept.
    expect(hub.to(revokePath)).toHaveLength(1);
    expect(store.saves).toEqual([]);
    expect(store.credential).toBeUndefined();
  });

  it("saves nothing and does not echo the bearer when a rotation fails after logout", async () => {
    const rotateStarted = deferred();
    const failure = deferred<Response>();
    const store = new RecordingStore(stored);
    const hub = stubHub({
      rotate: () => {
        rotateStarted.resolve();
        return failure.promise;
      },
    });
    const client = clientFor(store, hub);
    await client.discover();

    const rotation = outcome(client.rotateDevice());
    await rotateStarted.promise;
    await client.logout();
    failure.reject(new Error(`connection reset for ${token}`));

    const error = await rotation;
    expect(error).toBeInstanceOf(Error);
    expect(String(error)).not.toContain(token);
    expect(store.saves).toEqual([]);
    expect(store.credential).toBeUndefined();
  });
});

describe("rotation at the lifetime cap", () => {
  it("writes nothing when the answer states the stored expiry, sends one rotate and hands back the claim", async () => {
    const store = new RecordingStore(stored);
    const hub = stubHub({ rotate: () => maxLifetimeAnswer() });
    const client = clientFor(store, hub);

    const result = await client.rotateDevice();
    await settle();

    const claim: HubClaim = result.value;
    expect(claim.reason).toBe("max_lifetime");
    expect(claim).toMatchObject({ accessToken: token, deviceId: hubId, expiresAtMs });
    expect(hub.to(rotatePath)).toHaveLength(1);
    expect(hub.to(rotatePath)[0]?.headers.get("Authorization")).toBe(`Bearer ${token}`);
    expect(store.saves).toEqual([]);
    expect(store.credential).toEqual(stored);
    expect(hub.paths()).toEqual([discoveryPath, rotatePath]);
  });

  it("still saves a rotation that carries no reason", async () => {
    const store = new RecordingStore(stored);
    const hub = stubHub({ rotate: () => rotateAnswer() });
    const client = clientFor(store, hub);

    const result = await client.rotateDevice();

    expect(result.value.reason).toBeUndefined();
    expect(store.saves).toEqual([
      { accessToken: successorToken, deviceId: hubId, expiresAtMs: expiresAtMs + 1_000 },
    ]);
    expect(store.credential?.accessToken).toBe(successorToken);
  });

  it.each<[string, Record<string, unknown>, string]>([
    ["another bearer", { access_token: successorToken }, "HubClaim.accessToken"],
    ["another device", { device_id: otherHubId }, "HubClaim.deviceId"],
  ])(
    "rejects a max_lifetime answer that carries %s, saving nothing",
    async (_label, change, validator) => {
      const store = new RecordingStore(stored);
      const hub = stubHub({ rotate: () => maxLifetimeAnswer(change) });
      const client = clientFor(store, hub);

      await expect(client.rotateDevice()).rejects.toMatchObject({
        code: "protocol_validation",
        validator,
      });

      expect(hub.to(rotatePath)).toHaveLength(1);
      expect(store.saves).toEqual([]);
      expect(store.credential).toEqual(stored);
    },
  );

  it.each<[string, number]>([
    ["later than", expiresAtMs + 60_000],
    ["earlier than", expiresAtMs - 60_000],
  ])(
    "saves the expiry of an answer %s the stored one into the stored credential, bearer unchanged",
    async (_label, answerExpiresAtMs) => {
      const store = new RecordingStore(stored);
      const hub = stubHub({
        rotate: () => maxLifetimeAnswer({ expires_at_ms: answerExpiresAtMs }),
      });
      const client = clientFor(store, hub);

      const result = await client.rotateDevice();

      expect(result.value).toMatchObject({
        reason: "max_lifetime",
        accessToken: token,
        deviceId: hubId,
        expiresAtMs: answerExpiresAtMs,
      });
      expect(hub.to(rotatePath)).toHaveLength(1);
      expect(hub.to(rotatePath)[0]?.headers.get("Authorization")).toBe(`Bearer ${token}`);
      expect(store.saves).toEqual([
        { accessToken: token, deviceId: hubId, expiresAtMs: answerExpiresAtMs },
      ]);
      expect(store.credential).toEqual({
        accessToken: token,
        deviceId: hubId,
        expiresAtMs: answerExpiresAtMs,
      });
      expect(hub.paths()).toEqual([discoveryPath, rotatePath]);
    },
  );

  it("does not overwrite a credential that a claimPairing saved while the max_lifetime rotation was in flight", async () => {
    const answer = deferred<Response>();
    const rotateStarted = deferred();
    const store = new RecordingStore(stored);
    const hub = stubHub({
      claim: claimAnswer,
      rotate: () => {
        rotateStarted.resolve();
        return answer.promise;
      },
    });
    const client = clientFor(store, hub);
    await client.discover();

    const rotation = client.rotateDevice();
    await rotateStarted.promise;
    await client.claimPairing(pairingInvitation(), "Device");
    const claimed: HubCredential = { accessToken: "0".repeat(64), deviceId: hubId, expiresAtMs };
    expect(store.credential).toEqual(claimed);
    answer.resolve(maxLifetimeAnswer({ expires_at_ms: expiresAtMs + 60_000 }));
    const result = await rotation;

    // The caller still learns what the Hub said about the credential it presented ...
    expect(result.value).toMatchObject({
      reason: "max_lifetime",
      accessToken: token,
      expiresAtMs: expiresAtMs + 60_000,
    });
    // ... but the newer pairing wins: its credential is untouched and nothing else was written.
    expect(store.saves).toEqual([claimed]);
    expect(store.credential).toEqual(claimed);
  });

  it("writes nothing when the stored device id changed under the same bearer, or the store emptied, in flight", async () => {
    for (const replacement of [{ ...stored, deviceId: otherHubId }, undefined]) {
      const answer = deferred<Response>();
      const rotateStarted = deferred();
      const store = new RecordingStore(stored);
      const hub = stubHub({
        rotate: () => {
          rotateStarted.resolve();
          return answer.promise;
        },
      });
      const client = clientFor(store, hub);
      await client.discover();

      const rotation = client.rotateDevice();
      await rotateStarted.promise;
      store.credential = replacement;
      answer.resolve(maxLifetimeAnswer({ expires_at_ms: expiresAtMs + 60_000 }));
      await rotation;

      expect(store.saves).toEqual([]);
      expect(store.credential).toEqual(replacement);
    }
  });

  it("does not save the expiry of a max_lifetime answer that arrives after logout", async () => {
    const answer = deferred<Response>();
    const rotateStarted = deferred();
    const store = new RecordingStore(stored);
    const hub = stubHub({
      rotate: () => {
        rotateStarted.resolve();
        return answer.promise;
      },
    });
    const client = clientFor(store, hub);
    await client.discover();

    const rotation = outcome(client.rotateDevice());
    await rotateStarted.promise;
    await client.logout();
    answer.resolve(maxLifetimeAnswer({ expires_at_ms: expiresAtMs + 60_000 }));

    expect(await rotation).toMatchObject({ name: "AbortError" });
    expect(store.saves).toEqual([]);
    expect(store.credential).toBeUndefined();
  });

  it("does not read a different reason as the cap", async () => {
    const store = new RecordingStore(stored);
    const hub = stubHub({ rotate: () => maxLifetimeAnswer({ reason: "other" }) });
    const client = clientFor(store, hub);

    await expect(client.rotateDevice()).rejects.toMatchObject({ code: "protocol_validation" });

    expect(store.saves).toEqual([]);
  });
});

describe("logout re-entered from an abort listener", () => {
  it("queues its clear before the listener's logout, so a failed first clear cannot let a late rotation leave a successor", async () => {
    const events: string[] = [];
    const secondClearStarted = deferred();
    const secondClearGate = deferred();
    const rotateStarted = deferred();
    const lateAnswers = [deferred<Response>(), deferred<Response>()];
    const store = new RecordingStore(stored, events);
    store.onClear = async (call) => {
      if (call === 1) throw new Error("first clear failed");
      secondClearStarted.resolve();
      await secondClearGate.promise;
    };
    let rotateCalls = 0;
    let reentrant: Promise<unknown> | undefined;
    const hub = stubHub({
      events,
      // Both revokes fail: the local clears are all that is left.
      revoke: () => new Response(null, { status: 503 }),
      rotate: (request) => {
        const call = rotateCalls;
        rotateCalls += 1;
        if (call === 0) {
          // The abort that the first logout() sends runs this listener synchronously, before
          // that logout has moved on: a second logout() starts inside it.
          request.signal.addEventListener(
            "abort",
            () => {
              reentrant = outcome(client.logout());
            },
            { once: true },
          );
          rotateStarted.resolve();
        }
        return (lateAnswers[call] ?? lateAnswers[1])?.promise as Promise<Response>;
      },
    });
    const client = clientFor(store, hub);
    await client.discover();

    const firstRotation = outcome(client.rotateDevice());
    await rotateStarted.promise;
    const outer = outcome(client.logout());
    await secondClearStarted.promise;
    await settle();
    // The first clear failed, so the store still holds the credential, and the second clear is
    // held open. A rotation that starts now must wait for that second clear.
    expect(store.credential).toEqual(stored);
    await client.discover();
    const secondRotation = outcome(client.rotateDevice());
    for (let tick = 0; tick < 3; tick += 1) await settle();
    expect(rotateCalls).toBe(1);

    secondClearGate.resolve();
    // The Hub's answer to the rotation that logout aborted arrives late, and so would any other.
    lateAnswers[0]?.resolve(rotateAnswer());
    lateAnswers[1]?.resolve(rotateAnswer({ access_token: "c".repeat(64) }));

    expect(await outer).toMatchObject({ message: "first clear failed" });
    expect(await reentrant).toBeUndefined();
    expect(await firstRotation).toMatchObject({ name: "AbortError" });
    expect(await secondRotation).toMatchObject({ code: "hub_http_error", status: 401 });
    expect(hub.to(rotatePath)).toHaveLength(1);
    expect(hub.to(revokePath)).toHaveLength(2);
    for (const request of hub.to(revokePath)) {
      expect(request.headers.get("Authorization")).toBe(`Bearer ${token}`);
    }
    expect(events).toEqual(["revoke", "clear", "revoke", "clear"]);
    expect(store.clears).toBe(2);
    expect(store.saves).toEqual([]);
    expect(store.credential).toBeUndefined();
  });
});
