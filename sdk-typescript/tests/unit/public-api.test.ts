import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import vehicles from "../../protocol/source/examples/vehicles-page.json" with { type: "json" };
import { describe, expect, it } from "vitest";

type PublicApi = Record<string, unknown>;
type ClientFactory = (options: {
  readonly baseUrl: string;
  readonly authorization: () => undefined;
  readonly fetch: typeof fixtureFetch;
}) => Promise<{ listVehicles(): Promise<{ readonly kind: string; readonly value?: unknown }> }>;

describe("closed public API", () => {
  it("exports safe root values and runtime factories without low-level APIs", async () => {
    const root = (await import("@teslatlas/sdk")) as PublicApi;
    const browser = (await import("@teslatlas/sdk/browser")) as PublicApi;
    const node = (await import("@teslatlas/sdk/node")) as PublicApi;

    expect(root.TeslatlasError).toBeTypeOf("function");
    expect(root.asOpaqueCursor).toBeTypeOf("function");
    expect(root.createClient).toBeUndefined();
    expect(browser.createClient).toBeTypeOf("function");
    expect(node.createClient).toBeTypeOf("function");
    for (const api of [root, browser, node]) {
      expect(api.FetchTransport).toBeUndefined();
      expect(api.parseSseStream).toBeUndefined();
      expect(api.subscribeToSse).toBeUndefined();
    }
  });

  it("creates the same fixture client from both runtime entry points", async () => {
    const runtimes = [
      (await import("@teslatlas/sdk/browser")) as PublicApi,
      (await import("@teslatlas/sdk/node")) as PublicApi,
    ];

    for (const runtime of runtimes) {
      const createClient = runtime.createClient as ClientFactory;
      const client = await createClient({
        baseUrl: "https://fixture.invalid",
        authorization: () => undefined,
        fetch: fixtureFetch,
      });
      await expect(client.listVehicles()).resolves.toMatchObject({
        kind: "modified",
        value: { items: [expect.anything()] },
      });
    }
  });

  it.each([
    "@teslatlas/sdk/http/fetch-transport",
    "@teslatlas/sdk/generated/protocol",
    "@teslatlas/sdk/client/client",
  ])("rejects private deep import %s", async (specifier) => {
    await expect(import(specifier)).rejects.toThrow("is not exported");
  });
});

async function fixtureFetch(input: RequestInfo | URL): Promise<Response> {
  const url = input instanceof Request ? input.url : String(input);
  const path = new URL(url).pathname;
  if (path === "/.well-known/teslatlas-hub") {
    return Response.json(discovery, { headers: { ETag: 'W/"fixture-discovery-1"' } });
  }
  if (path === "/v1/vehicles") {
    return Response.json(vehicles, { headers: { ETag: 'W/"fixture-vehicles-1"' } });
  }
  throw new Error(`Unexpected fixture path ${path}`);
}
