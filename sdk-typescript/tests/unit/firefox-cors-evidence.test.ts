import { describe, expect, it, vi } from "vitest";
import { observeFirefoxCorsReads } from "../../scripts/firefox-cors-evidence.mjs";

const appOrigin = new URL("http://127.0.0.1:4174/");
const hubEndpoint = "https://127.0.0.1:21444";

describe("Firefox CORS evidence", () => {
  it("uses context request evidence without inventing a Firefox preflight count", async () => {
    const routeResult = {
      defaultFetch: true,
      ok: true,
      pageOrigin: appOrigin.origin,
      semanticSnapshot: { schema_version: 1 },
      semanticSnapshotCanonicalization: "sorted-json-v1",
      semanticSnapshotSha256: "a".repeat(64),
      vehicleCount: 1,
    };
    let requestListener: ((request: { method(): string; url(): string }) => void) | undefined;
    const page = {
      close: vi.fn(async () => undefined),
      evaluate: vi.fn(async () => routeResult),
      goto: vi.fn(async () => {
        requestListener?.({ method: () => "GET", url: () => `${hubEndpoint}/v1/vehicles` });
      }),
      waitForFunction: vi.fn(async () => undefined),
    };
    const context = {
      browser: () => ({ version: () => "firefox-test" }),
      newPage: vi.fn(async () => page),
      off: vi.fn(),
      on: vi.fn((_event: "request", listener: typeof requestListener) => {
        requestListener = listener;
      }),
    };

    const result = await observeFirefoxCorsReads(context, appOrigin, hubEndpoint);

    expect(result).toMatchObject({
      cors: {
        browserEnforced: true,
        hubRequestsObserved: 1,
        preflightVisibility: "not-exposed-by-playwright-firefox-request-events",
      },
      defaultFetch: true,
      runtime: "firefox-test",
      semanticSnapshot: { schema_version: 1 },
      semanticSnapshotCanonicalization: "sorted-json-v1",
      semanticSnapshotSha256: "a".repeat(64),
    });
    expect(result).not.toHaveProperty("corsPreflightCount");
    expect(context.on).toHaveBeenCalledWith("request", expect.any(Function));
    expect(context.off).toHaveBeenCalledWith("request", requestListener);
    expect(page.close).toHaveBeenCalledOnce();
  });

  it("fails when no cross-origin Hub application request is observable", async () => {
    const page = {
      close: vi.fn(async () => undefined),
      evaluate: vi.fn(async () => ({ defaultFetch: true, ok: true, pageOrigin: appOrigin.origin })),
      goto: vi.fn(async () => undefined),
      waitForFunction: vi.fn(async () => undefined),
    };
    const context = {
      browser: () => ({ version: () => "firefox-test" }),
      newPage: vi.fn(async () => page),
      off: vi.fn(),
      on: vi.fn(),
    };

    await expect(observeFirefoxCorsReads(context, appOrigin, hubEndpoint)).rejects.toThrow(
      "no cross-origin Hub requests",
    );
    expect(page.close).toHaveBeenCalledOnce();
  });
});
