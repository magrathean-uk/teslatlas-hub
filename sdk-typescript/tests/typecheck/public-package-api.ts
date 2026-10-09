import type { HubClient, HubCredential, HubDiscovery } from "@teslatlas/sdk";
import { createHubClient as createNodeHubClient } from "@teslatlas/sdk/node";

const credential: HubCredential = {
  accessToken: "0".repeat(64),
  deviceId: "11111111-1111-4111-8111-111111111111",
  expiresAtMs: 1_900_000_000_000,
};
const hubClient: HubClient = createNodeHubClient({
  endpoint: "https://hub.example.invalid",
  expectedHubId: "11111111-1111-4111-8111-111111111111",
  credentials: {
    load: () => credential,
    save: () => undefined,
    clear: () => undefined,
  },
});
const discovery: Promise<HubDiscovery> = hubClient.discover().then((result) => result.value);
const nodeHubClient: HubClient = createNodeHubClient({
  endpoint: "https://hub.example.invalid",
  expectedHubId: "11111111-1111-4111-8111-111111111111",
  credentials: {
    load: () => credential,
    save: () => undefined,
    clear: () => undefined,
  },
  nodeTls: { ca: "test CA bytes" },
});
void [discovery, nodeHubClient];
