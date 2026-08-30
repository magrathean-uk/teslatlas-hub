import { createBrowserTransport, subscribeToSse } from "../../src/browser.js";
import { defineTransportConformanceSuite } from "./shared-transport-suite.js";

defineTransportConformanceSuite("browser", {
  createTransport: createBrowserTransport,
  subscribeToSse,
});
