import { describe, expect, it, vi } from "vitest";
import { createProvisionedBrowserClientAndCheckPairing } from "../../scripts/browser-pairing-preflight.mjs";
import { createHubClient } from "../../src/browser.js";

const endpoint = "https://hub.example.invalid";
const hubId = "11111111-1111-4111-8111-111111111111";
const credential = {
  accessToken: "a".repeat(64),
  deviceId: hubId,
  expiresAtMs: 1_900_000_000_000,
};
const credentials = {
  load: () => credential,
  save: () => undefined,
  clear: () => undefined,
};

describe("real-browser pairing preflight", () => {
  it("uses the public browser factory and fails before fetch without a pin-capable transport", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>();
    const options: Array<Parameters<typeof createHubClient>[0]> = [];
    const observedFactory = (value: Parameters<typeof createHubClient>[0]) => {
      options.push(value);
      return createHubClient(value);
    };

    const result = await createProvisionedBrowserClientAndCheckPairing({
      createHubClient: observedFactory,
      endpoint,
      hubId,
      credentials,
      fetch,
    });

    expect(result.pairing).toEqual({
      errorCode: "hub_tls_pin_unavailable",
      networkRequestCount: 0,
    });
    expect(fetch).not.toHaveBeenCalled();
    expect(result.readsUseDefaultFetch).toBe(true);
    expect(options).toHaveLength(2);
    expect(options[0]?.fetch).toBeTypeOf("function");
    expect(options[1]?.fetch).toBeUndefined();
    result.client.dispose();
  });

  it("rejects a factory that dispatches during the pairing preflight", async () => {
    const dispose = vi.fn();
    const fetch = vi.fn<typeof globalThis.fetch>().mockResolvedValue(new Response());
    const unsafeFactory = ((options: Parameters<typeof createHubClient>[0]) => ({
      claimPairing: async () => {
        await options.fetch?.(endpoint);
        throw Object.assign(new Error("unavailable"), { code: "hub_tls_pin_unavailable" });
      },
      dispose,
    })) as unknown as typeof createHubClient;

    await expect(
      createProvisionedBrowserClientAndCheckPairing({
        createHubClient: unsafeFactory,
        endpoint,
        hubId,
        credentials,
        fetch,
      }),
    ).rejects.toThrow("dispatched network I/O");
    expect(fetch).toHaveBeenCalledOnce();
    expect(dispose).toHaveBeenCalledOnce();
  });
});
