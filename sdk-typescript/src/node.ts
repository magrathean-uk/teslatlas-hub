import { FetchTransport, type FetchTransportOptions } from "./http/fetch-transport.js";

export * from "./index.js";

export function createNodeTransport(options: FetchTransportOptions): FetchTransport {
  return new FetchTransport(options);
}
