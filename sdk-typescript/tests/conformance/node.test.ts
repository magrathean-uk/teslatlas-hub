import { createNodeTransport, subscribeToSse } from "../../src/node.js";
import { defineTransportConformanceSuite } from "./shared-transport-suite.js";

defineTransportConformanceSuite("node", {
  createTransport: createNodeTransport,
  subscribeToSse,
});
