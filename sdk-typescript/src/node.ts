import { createHubClient as createStaticHubClient } from "./hub/client.js";
import { createNodeHubClaimTransport, type NodeHubTlsOptions } from "./hub/node-claim-transport.js";
import type { CreateHubClientOptions, HubClient } from "./hub/models.js";

export * from "./index.js";
export type { NodeHubTlsOptions } from "./hub/node-claim-transport.js";

export interface CreateNodeHubClientOptions extends CreateHubClientOptions {
  readonly nodeTls?: NodeHubTlsOptions;
}

export function createHubClient(options: CreateNodeHubClientOptions): HubClient {
  return createStaticHubClient({
    ...options,
    claimTransport: options.claimTransport ?? createNodeHubClaimTransport(options.nodeTls),
  });
}
