import type { AuthorizationProvider } from "../auth/credential-store.js";
import type { FetchTransport, FetchImplementation } from "../http/fetch-transport.js";
import type { HubDescriptor } from "../protocol/models.js";
import type { SupportedProtocolVersion } from "../protocol/negotiation.js";

export interface CreateClientOptions {
  readonly baseUrl: string | URL;
  readonly authorization: AuthorizationProvider;
  readonly fetch?: FetchImplementation;
  readonly requestedProtocolVersion?: SupportedProtocolVersion;
  readonly signal?: AbortSignal;
}

export interface ClientSession {
  readonly descriptor: HubDescriptor;
  readonly protocolVersion: SupportedProtocolVersion;
  readonly discoveryTransport: FetchTransport;
  readonly apiTransport: FetchTransport;
  readonly eventTransport: FetchTransport;
}
