import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import { expect, it } from "vitest";
import { createBrowserTransport, subscribeToSse } from "../../src/browser.js";
import { TeslatlasClient } from "../../src/client/client.js";
import { createClientSession } from "../../src/client/session.js";
import { validateDiscovery } from "../../src/generated/validators.js";
import { defineTransportConformanceSuite } from "./shared-transport-suite.js";
import { defineTypedClientConformanceSuite } from "./typed-client-suite.js";

defineTransportConformanceSuite("browser", {
  createTransport: createBrowserTransport,
  subscribeToSse,
});

defineTypedClientConformanceSuite("browser", {
  createClient: async (options) => new TeslatlasClient(await createClientSession(options)),
});

it("imports generated standalone validators without a CommonJS runtime", () => {
  expect(validateDiscovery(discovery)).toBe(true);
});
