import { TeslatlasClient } from "./client/client.js";
import { createClientSession, type CreateClientOptions } from "./client/session.js";

export * from "./index.js";

export async function createClient(options: CreateClientOptions): Promise<TeslatlasClient> {
  return new TeslatlasClient(await createClientSession(options));
}
