import {
  createClient as createBrowserClient,
  createHubClient as createStaticHubClient,
} from "./browser.js";
import type { TeslatlasClient } from "./client/client.js";
import type { CreateClientOptions } from "./client/session.js";
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

export async function createClient(options: CreateClientOptions): Promise<TeslatlasClient> {
  return createBrowserClient(options);
}
