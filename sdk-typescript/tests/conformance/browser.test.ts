import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import { expect, it } from "vitest";
import { createClient } from "../../src/browser.js";
import { validateDiscovery } from "../../src/generated/validators.js";
import { defineTransportConformanceSuite } from "./shared-transport-suite.js";
import { defineTypedClientConformanceSuite } from "./typed-client-suite.js";

defineTransportConformanceSuite("browser", {
  createClient,
});

defineTypedClientConformanceSuite("browser", {
  createClient,
});

it("imports generated standalone validators without a CommonJS runtime", () => {
  expect(validateDiscovery(discovery)).toBe(true);
});
