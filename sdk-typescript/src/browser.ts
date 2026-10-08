import { TeslatlasClient } from "./client/client.js";
import { createClientSession, type CreateClientOptions } from "./client/session.js";
import { createHubClient as createStaticHubClient } from "./hub/client.js";
import type { CreateHubClientOptions, HubClient } from "./hub/models.js";

export * from "./index.js";

export function createHubClient(options: CreateHubClientOptions): HubClient {
  return createStaticHubClient(options);
}

export async function createClient(options: CreateClientOptions): Promise<TeslatlasClient> {
  return new TeslatlasClient(await createClientSession(options));
}
