import { FetchTransport, type FetchTransportOptions } from "./http/fetch-transport.js";

export * from "./index.js";

export function createBrowserTransport(options: FetchTransportOptions): FetchTransport {
  return new FetchTransport(options);
}
