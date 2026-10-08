import { createClient } from "../../src/node.js";
import { defineTransportConformanceSuite } from "./shared-transport-suite.js";
import { defineTypedClientConformanceSuite } from "./typed-client-suite.js";

defineTransportConformanceSuite("node", {
  createClient,
});

defineTypedClientConformanceSuite("node", {
  createClient,
});
