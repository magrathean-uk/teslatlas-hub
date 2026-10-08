import { afterEach, describe, expect, it, vi } from "vitest";
import {
  FirefoxNssCleanupError,
  withFirefoxNssProfile,
} from "../../scripts/firefox-nss-profile.mjs";
import {
  fetchResponseWithDeadline,
  readBoundedResponseBytes,
  requestSignedSyncJson,
} from "../../scripts/hub-sync-contract-evidence.mjs";
import {
  assertNoForbiddenSemanticData,
  selectSemanticSnapshotPrimaryVehicle,
} from "../../scripts/redacted-semantic-snapshot.mjs";

afterEach(() => vi.useRealTimers());

const v7 = "018f18d2-6f45-7b3c-8a91-3c7286a10d42";
const v4 = "11111111-1111-4111-8111-111111111111";
const profile = (primaryVehicleId: string) => ({
  schemaVersion: 1 as const,
  primaryVehicleId,
  vehicleCount: 1,
  history: { fromMs: 1, toMs: 3 },
  boundaryMode: "derived-latest-drive" as const,
});

function d9Deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

function d9Fixture() {
  const profilePath = "/owned/teslatlas-firefox-nss-original";
  const close = vi.fn(async () => {});
  const context = { close };
  const launch = vi.fn(async () => context);
  const remove = vi.fn(async () => {});
  const metadata = {
    parentIno: 10,
    profileIno: 20,
    uid: process.getuid?.() ?? 0,
    parentUid: process.getuid?.() ?? 0,
    dev: 1,
    symlink: false,
    missing: false,
    parentCanonical: "/owned",
    profileCanonical: profilePath,
  };
  const deps = {
    access: vi.fn(async () => {}),
    chmod: vi.fn(async () => {}),
    mkdtemp: vi.fn(async () => profilePath),
    execFile: vi.fn(async () => ({})),
    rm: remove,
    realpath: vi.fn(async (path: string) =>
      path === "/owned"
        ? metadata.parentCanonical
        : path === profilePath
          ? metadata.profileCanonical
          : path,
    ),
    lstat: vi.fn(async (path: string) => {
      if (path === profilePath && metadata.missing)
        throw Object.assign(new Error("absent"), { code: "ENOENT" });
      return {
        dev: metadata.dev,
        ino: path === "/owned" ? metadata.parentIno : metadata.profileIno,
        uid: path === "/owned" ? metadata.parentUid : metadata.uid,
        mode: 0o700,
        isSymbolicLink: () => path === profilePath && metadata.symlink,
        isFile: () => path.includes("."),
        isDirectory: () => !path.includes("."),
      };
    }),
  };
  const invoke = (
    operation: (value: { close(): Promise<void> }) => Promise<string> = async () => "value",
  ) =>
    withFirefoxNssProfile(
      {
        certificatePath: "/owned/ca.pem",
        temporaryParent: "/owned",
        closeTimeoutMs: 10,
        certutilTimeoutMs: 40,
        firefox: { launchPersistentContext: launch },
      },
      operation,
      deps,
    );
  return { profilePath, close, context, launch, remove, metadata, deps, invoke };
}

async function d9Error(promise: Promise<unknown>): Promise<FirefoxNssCleanupError> {
  const value = await promise.catch((error: unknown) => error);
  expect(value).toBeInstanceOf(FirefoxNssCleanupError);
  return value as FirefoxNssCleanupError;
}

describe("D9 owned profile preservation and retry", () => {
  it("retains the normal success shape and strict initialization", async () => {
    const f = d9Fixture();
    await expect(f.invoke()).resolves.toMatchObject({
      value: "value",
      evidence: { profileCleanup: "removed", enterpriseRoots: false, ignoreHTTPSErrors: false },
    });
    expect(f.launch).toHaveBeenCalledWith(f.profilePath, {
      headless: true,
      firefoxUserPrefs: { "security.enterprise_roots.enabled": false },
      ignoreHTTPSErrors: false,
    });
    expect(f.deps.execFile).toHaveBeenCalledTimes(3);
    expect(f.close).toHaveBeenCalledOnce();
    expect(f.remove).toHaveBeenCalledExactlyOnceWith(f.profilePath, {
      recursive: true,
      force: true,
    });
    expect(f.remove.mock.invocationCallOrder[0]).toBeGreaterThan(
      f.close.mock.invocationCallOrder[0] ?? Number.POSITIVE_INFINITY,
    );
  });
  it("preserves the original operation error unchanged when cleanup succeeds", async () => {
    const f = d9Fixture();
    const primary = new Error("operation failed");
    await expect(
      f.invoke(async () => {
        throw primary;
      }),
    ).rejects.toBe(primary);
    expect(f.remove).toHaveBeenCalledOnce();
  });
  it("preserves primary and close failures in order with immutable cleanup authority", async () => {
    const f = d9Fixture();
    const primary = new Error("operation failed");
    const closing = new Error("close failed");
    f.close.mockRejectedValue(closing);
    const error = await d9Error(
      f.invoke(async () => {
        throw primary;
      }),
    );
    expect(error.errors).toEqual([primary, closing]);
    expect(error.cause).toBe(primary);
    expect(error.deferredCleanup).toMatchObject({
      profilePath: f.profilePath,
      state: "deferred",
      reason: "close_rejected",
    });
    expect(Object.isFrozen(error.deferredCleanup)).toBe(true);
    expect(Reflect.set(error.deferredCleanup, "profilePath", "/foreign")).toBe(false);
    expect(Reflect.set(error, "deferredCleanup", {})).toBe(false);
    expect(f.remove).not.toHaveBeenCalled();
  });
  it("permits one new close attempt after rejection and then idempotent cleanup", async () => {
    const f = d9Fixture();
    const first = new Error("first close");
    f.close.mockRejectedValueOnce(first).mockResolvedValue(undefined);
    const error = await d9Error(f.invoke());
    expect(error.errors).toEqual([first]);
    expect(error.cause).toBe(first);
    await error.deferredCleanup.retry();
    await error.deferredCleanup.retry();
    expect(f.close).toHaveBeenCalledTimes(2);
    expect(f.remove).toHaveBeenCalledOnce();
    expect(error.deferredCleanup.state).toBe("removed");
  });
  it("does not delete on late close fulfillment and uses the same attempt on explicit retry", async () => {
    vi.useFakeTimers();
    const f = d9Fixture();
    const close = d9Deferred<void>();
    const entered = d9Deferred<void>();
    f.close.mockImplementation(() => {
      entered.resolve();
      return close.promise;
    });
    const pending = d9Error(f.invoke());
    await entered.promise;
    await vi.advanceTimersByTimeAsync(10);
    const error = await pending;
    expect(error.deferredCleanup.reason).toBe("close_timed_out");
    expect(f.remove).not.toHaveBeenCalled();
    const retry = d9Error(error.deferredCleanup.retry());
    await vi.advanceTimersByTimeAsync(10);
    await retry;
    expect(f.close).toHaveBeenCalledOnce();
    expect(f.remove).not.toHaveBeenCalled();
    close.resolve();
    await close.promise;
    await Promise.resolve();
    expect(f.remove).not.toHaveBeenCalled();
    await error.deferredCleanup.retry();
    expect(f.close).toHaveBeenCalledOnce();
    expect(f.remove).toHaveBeenCalledOnce();
  });
  it("serializes concurrent retry deadlines without duplicating close or removal", async () => {
    vi.useFakeTimers();
    const f = d9Fixture();
    const close = d9Deferred<void>();
    const entered = d9Deferred<void>();
    f.close.mockImplementation(() => {
      entered.resolve();
      return close.promise;
    });
    const pending = d9Error(f.invoke());
    await entered.promise;
    await vi.advanceTimersByTimeAsync(10);
    const error = await pending;
    const first = d9Error(error.deferredCleanup.retry());
    const second = error.deferredCleanup.retry();
    const timers = vi.getTimerCount();
    await vi.advanceTimersByTimeAsync(10);
    await first;
    expect(vi.getTimerCount()).toBeLessThanOrEqual(1);
    expect(timers).toBeLessThanOrEqual(1);
    close.resolve();
    await second;
    await error.deferredCleanup.retry();
    expect(f.close).toHaveBeenCalledOnce();
    expect(f.remove).toHaveBeenCalledOnce();
    expect(vi.getTimerCount()).toBe(0);
  });
  it("keeps removal failure visible and retryable without another close", async () => {
    const f = d9Fixture();
    const failure = new Error("cannot remove");
    f.remove.mockRejectedValueOnce(failure).mockResolvedValue(undefined);
    const error = await d9Error(f.invoke());
    expect(error.errors).toEqual([failure]);
    expect(error.deferredCleanup.reason).toBe("remove_failed");
    await error.deferredCleanup.retry();
    expect(f.close).toHaveBeenCalledOnce();
    expect(f.remove).toHaveBeenCalledTimes(2);
  });
  it.each([
    "inode",
    "uid",
    "parent_inode",
    "parent_uid",
    "parent_path",
    "profile_path",
    "symlink",
  ] as const)("refuses a changed %s before removal", async (change) => {
    const f = d9Fixture();
    f.close.mockRejectedValueOnce(new Error("defer")).mockResolvedValue(undefined);
    const error = await d9Error(f.invoke());
    if (change === "inode") f.metadata.profileIno++;
    if (change === "uid") f.metadata.uid++;
    if (change === "parent_inode") f.metadata.parentIno++;
    if (change === "parent_uid") f.metadata.parentUid++;
    if (change === "parent_path") f.metadata.parentCanonical = "/foreign";
    if (change === "profile_path") f.metadata.profileCanonical = "/foreign";
    if (change === "symlink") f.metadata.symlink = true;
    await expect(error.deferredCleanup.retry()).rejects.toBeInstanceOf(FirefoxNssCleanupError);
    expect(error.deferredCleanup.reason).toBe("ownership_changed");
    expect(f.remove).not.toHaveBeenCalled();
  });
  it("does not trust a missing profile until close has fulfilled and parent still matches", async () => {
    const f = d9Fixture();
    f.close
      .mockRejectedValueOnce(new Error("defer"))
      .mockRejectedValueOnce(new Error("still open"))
      .mockResolvedValue(undefined);
    const error = await d9Error(f.invoke());
    f.metadata.missing = true;
    await expect(error.deferredCleanup.retry()).rejects.toBeInstanceOf(FirefoxNssCleanupError);
    expect(error.deferredCleanup.state).toBe("deferred");
    await error.deferredCleanup.retry();
    expect(error.deferredCleanup.state).toBe("removed");
    expect(f.remove).not.toHaveBeenCalled();
  });
  it("preserves the profile when launch rejects without an acquired context", async () => {
    const f = d9Fixture();
    const primary = new Error("launch did not return context");
    f.launch.mockRejectedValue(primary);
    const error = await d9Error(f.invoke());
    expect(error.cause).toBe(primary);
    expect(error.errors[0]).toBe(primary);
    expect(error.deferredCleanup.reason).toBe("settlement_unknown");
    await expect(error.deferredCleanup.retry()).rejects.toBeInstanceOf(FirefoxNssCleanupError);
    expect(f.close).not.toHaveBeenCalled();
    expect(f.launch).toHaveBeenCalledOnce();
    expect(f.remove).not.toHaveBeenCalled();
  });
  it("keeps pre-browser initialization failure separate and cleans only owned storage", async () => {
    const f = d9Fixture();
    const primary = new Error("initialization failed");
    f.deps.execFile.mockRejectedValueOnce(primary);
    await expect(f.invoke()).rejects.toBe(primary);
    expect(f.launch).not.toHaveBeenCalled();
    expect(f.close).not.toHaveBeenCalled();
    expect(f.remove).toHaveBeenCalledOnce();
  });
  it("binds the acquired close method before caller operation can replace it", async () => {
    const f = d9Fixture();
    const replacement = vi.fn(async () => {});
    await f.invoke(async (context) => {
      context.close = replacement;
      return "value";
    });
    expect(f.close).toHaveBeenCalledOnce();
    expect(replacement).not.toHaveBeenCalled();
  });
});

describe("next tooling acceptance", () => {
  it.each([v4, v7])("selects a canonical primary identity %s", (id) => {
    expect(selectSemanticSnapshotPrimaryVehicle([{ vehicleId: id }], profile(id))).toBe(id);
  });
  it.each([`prefix ${v7}`, `${v7} suffix`, v7.toUpperCase(), "not-a-uuid"])(
    "rejects noncanonical profile input %s",
    (id) => {
      expect(() => selectSemanticSnapshotPrimaryVehicle([{ vehicleId: id }], profile(id))).toThrow(
        "must be a UUID",
      );
    },
  );
  it.each([v4, v7, v7.toUpperCase()])("redacts embedded UUID text %s", (id) => {
    expect(() =>
      assertNoForbiddenSemanticData({ current: { state: `online ${id} observed` } }),
    ).toThrow("forbidden string");
    expect(() => assertNoForbiddenSemanticData({ current: { state: "online" } })).not.toThrow();
  });

  it.each([`online_${v7}_cached`, `prefix${v7}suffix`])(
    "redacts UUID substrings adjacent to word characters %s",
    (state) => {
      expect(() => assertNoForbiddenSemanticData({ current: { state } })).toThrow(
        "forbidden string",
      );
    },
  );

  it("rejects advertised oversize before pulling and cancels the rejected body", async () => {
    const pull = vi.fn();
    const cancel = vi.fn();
    const response = new Response(new ReadableStream({ pull, cancel }, { highWaterMark: 0 }), {
      headers: { "content-length": String(6 * 1024 * 1024) },
    });
    await expect(
      requestSignedSyncJson({
        endpoint: new URL("https://fixture.invalid"),
        path: "/manifest",
        authorization: {},
        signaturePublicKey: "unused",
        fetch: async () => response,
      }),
    ).rejects.toThrow("bounded content length");
    expect(pull).not.toHaveBeenCalled();
    expect(cancel).toHaveBeenCalledOnce();
  });

  it("stops dishonest-length JSON and pack streams at the first excess chunk", async () => {
    for (const expectedBytes of [undefined, 4]) {
      let pulls = 0;
      const cancel = vi.fn();
      const response = new Response(
        new ReadableStream(
          {
            pull(controller) {
              pulls++;
              controller.enqueue(new Uint8Array(3));
            },
            cancel,
          },
          { highWaterMark: 0 },
        ),
        { headers: { "content-length": "4" } },
      );
      await expect(
        readBoundedResponseBytes(response, {
          label: "fixture",
          maxBytes: 4,
          ...(expectedBytes === undefined ? {} : { expectedBytes }),
        }),
      ).rejects.toThrow("bounded body limit");
      expect(pulls).toBe(2);
      expect(cancel).toHaveBeenCalledOnce();
      expect(response.body?.locked).toBe(false);
    }
  });

  it("accepts heavily fragmented bytes without chunk retention and stops endless empty chunks", async () => {
    const bytes = Uint8Array.from({ length: 4_096 }, (_, index) => index % 256);
    let offset = 0;
    const fragmented = new Response(
      new ReadableStream(
        {
          pull(controller) {
            if (offset === bytes.length) controller.close();
            else controller.enqueue(bytes.subarray(offset, ++offset));
          },
        },
        { highWaterMark: 0 },
      ),
    );
    await expect(
      readBoundedResponseBytes(fragmented, { label: "fragmented", maxBytes: bytes.length }),
    ).resolves.toEqual(bytes);
    let pulls = 0;
    const cancel = vi.fn();
    const empty = new Response(
      new ReadableStream(
        {
          pull(controller) {
            pulls++;
            controller.enqueue(new Uint8Array());
          },
          cancel,
        },
        { highWaterMark: 0 },
      ),
    );
    await expect(readBoundedResponseBytes(empty, { label: "empty", maxBytes: 4 })).rejects.toThrow(
      "empty chunk bound",
    );
    expect(pulls).toBe(1_025);
    expect(cancel).toHaveBeenCalledOnce();
  });

  it("returns exact valid chunks and rejects truncated pack length", async () => {
    const bytes = new Uint8Array([1, 2, 3, 4]);
    await expect(
      readBoundedResponseBytes(new Response(bytes, { headers: { "content-length": "4" } }), {
        label: "pack",
        maxBytes: 8,
        expectedBytes: 4,
        requireContentLength: true,
      }),
    ).resolves.toEqual(bytes);
    await expect(
      readBoundedResponseBytes(new Response(bytes.subarray(0, 3)), {
        label: "pack",
        maxBytes: 8,
        expectedBytes: 4,
      }),
    ).rejects.toThrow("stream length differs");
    await expect(
      readBoundedResponseBytes(new Response(bytes), {
        label: "pack",
        maxBytes: 8,
        expectedBytes: 4,
        requireContentLength: true,
      }),
    ).rejects.toThrow("requires content length");
  });

  it("cancels a rejected status without pulling or awaiting cancellation", async () => {
    const pull = vi.fn();
    const cancel = vi.fn(() => new Promise<void>(() => {}));
    const response = new Response(new ReadableStream({ pull, cancel }, { highWaterMark: 0 }), {
      status: 503,
    });
    await expect(
      readBoundedResponseBytes(response, { label: "fixture", maxBytes: 4 }),
    ).rejects.toThrow("HTTP 503");
    expect(pull).not.toHaveBeenCalled();
    expect(cancel).toHaveBeenCalledOnce();
  });

  it("times out a pending read and initiates cancellation", async () => {
    vi.useFakeTimers();
    const cancel = vi.fn();
    const response = new Response(
      new ReadableStream({ pull: () => new Promise<void>(() => {}), cancel }, { highWaterMark: 0 }),
    );
    const result = readBoundedResponseBytes(response, {
      label: "fixture",
      maxBytes: 4,
      timeoutMs: 50,
    });
    const assertion = expect(result).rejects.toThrow("body read timed out");
    await vi.advanceTimersByTimeAsync(50);
    await assertion;
    expect(cancel).toHaveBeenCalledOnce();
  });

  it("bounds a pending fetch, aborts it, and cancels a late response", async () => {
    vi.useFakeTimers();
    let settle!: (value: Response) => void;
    let signal: AbortSignal | null | undefined;
    const fetch = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      signal = init?.signal;
      return new Promise<Response>((resolve) => {
        settle = resolve;
      });
    });
    const result = fetchResponseWithDeadline("https://fixture.invalid", {}, fetch, 50);
    const assertion = expect(result).rejects.toThrow("evidence fetch timed out");
    await vi.advanceTimersByTimeAsync(50);
    await assertion;
    expect(signal?.aborted).toBe(true);
    let signalCancellation!: () => void;
    const cancelled = new Promise<void>((resolve) => {
      signalCancellation = resolve;
    });
    const cancel = vi.fn(() => signalCancellation());
    settle(new Response(new ReadableStream({ cancel }, { highWaterMark: 0 })));
    await cancelled;
    expect(cancel).toHaveBeenCalledOnce();
  });

  it("preserves the exact owned profile after pending close times out and reports deferred failure", async () => {
    vi.useFakeTimers();
    let signalClose!: () => void;
    const closing = new Promise<void>((resolve) => {
      signalClose = resolve;
    });
    const remove = vi.fn(async () => undefined);
    const execute = vi.fn(async (_path: string, _args: string[], options: unknown) => {
      expect(options).toMatchObject({ timeout: 40, killSignal: "SIGKILL", shell: false });
      return {};
    });
    const result = withFirefoxNssProfile(
      {
        certificatePath: "/owned/ca.pem",
        temporaryParent: "/owned",
        closeTimeoutMs: 50,
        certutilTimeoutMs: 40,
        firefox: {
          launchPersistentContext: async () => ({
            close: () => {
              signalClose();
              return new Promise<void>(() => {});
            },
          }),
        },
      },
      async () => "success",
      {
        access: async () => undefined,
        chmod: async () => undefined,
        realpath: async (path: string) => path,
        lstat: async (path: string) => ({
          uid: process.getuid?.(),
          dev: 1,
          ino: path === "/owned" ? 1 : 2,
          mode: 0o700,
          isSymbolicLink: () => false,
          isFile: () => path.includes("."),
          isDirectory: () => !path.includes("."),
        }),
        mkdtemp: async () => "/owned/teslatlas-firefox-nss-test",
        rm: remove,
        execFile: execute,
      },
    );
    const outcome = result.catch((error: unknown) => error);
    await closing;
    expect(remove).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(50);
    const error = await outcome;
    expect(error).toBeInstanceOf(AggregateError);
    expect(error).toMatchObject({
      deferredCleanup: {
        profilePath: "/owned/teslatlas-firefox-nss-test",
        state: "deferred",
        reason: "close_timed_out",
      },
    });
    expect(remove).not.toHaveBeenCalled();
    expect(execute).toHaveBeenCalledTimes(3);
  });
});
