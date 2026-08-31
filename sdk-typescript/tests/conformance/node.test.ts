import { createNodeTransport, subscribeToSse } from "../../src/node.js";
import { TeslatlasClient } from "../../src/client/client.js";
import { createClientSession } from "../../src/client/session.js";
import { defineTransportConformanceSuite } from "./shared-transport-suite.js";
import { defineTypedClientConformanceSuite } from "./typed-client-suite.js";

defineTransportConformanceSuite("node", {
  createTransport: createNodeTransport,
  subscribeToSse,
});

defineTypedClientConformanceSuite("node", {
  createClient: async (options) => new TeslatlasClient(await createClientSession(options)),
});
