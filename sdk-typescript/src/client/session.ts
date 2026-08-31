import { ProtocolValidationError } from "../core/errors.js";
import { validateDiscovery } from "../generated/validators.js";
import { readEntityTag } from "../http/conditional.js";
import { FetchTransport, InvalidBaseUrlError } from "../http/fetch-transport.js";
import type { HubDescriptor } from "../protocol/models.js";
import { negotiateProtocolVersion } from "../protocol/negotiation.js";
import { decodeProtocolValue } from "../protocol/validate.js";
import type { ClientSession, CreateClientOptions } from "./types.js";

const discoveryPath = "/.well-known/teslatlas-hub";
const currentProtocolVersion = "1.2.0";

export type { ClientSession, CreateClientOptions } from "./types.js";

export async function createClientSession(options: CreateClientOptions): Promise<ClientSession> {
  if (!isSecureEndpointUrl(options.baseUrl)) {
    throw new InvalidBaseUrlError();
  }
  const discoveryTransport = new FetchTransport({
    baseUrl: options.baseUrl,
    ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
  });
  const response = await discoveryTransport.request(discoveryPath, {
    redirect: "error",
    ...(options.signal === undefined ? {} : { signal: options.signal }),
  });
  if (response.status !== 200) {
    throw new ProtocolValidationError("Discovery.status");
  }
  requireDiscoveryEntityTag(response);
  const descriptor = decodeProtocolValue<HubDescriptor>(
    await readDiscoveryBody(response, options.signal),
    validateDiscovery,
    "validateDiscovery",
  );
  validateEndpointUrls(descriptor);

  const protocolVersion = negotiateProtocolVersion(
    descriptor,
    options.requestedProtocolVersion ?? currentProtocolVersion,
  );
  const sharedTransportOptions = {
    authorization: options.authorization,
    ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
  };

  return Object.freeze({
    descriptor,
    protocolVersion,
    discoveryTransport,
    apiTransport: new FetchTransport({
      ...sharedTransportOptions,
      baseUrl: descriptor.endpoints.api,
    }),
    eventTransport: new FetchTransport({
      ...sharedTransportOptions,
      baseUrl: descriptor.endpoints.events,
    }),
  });
}

function requireDiscoveryEntityTag(response: Response): void {
  try {
    if (readEntityTag(response.headers) === undefined) {
      throw new ProtocolValidationError("Discovery.etag");
    }
  } catch {
    throw new ProtocolValidationError("Discovery.etag");
  }
}

async function readDiscoveryBody(
  response: Response,
  signal: AbortSignal | undefined,
): Promise<unknown> {
  try {
    return await response.json();
  } catch (error) {
    if (signal?.aborted === true) {
      throw signal.reason ?? error;
    }
    throw new ProtocolValidationError("validateDiscovery");
  }
}

function validateEndpointUrls(descriptor: HubDescriptor): void {
  for (const value of Object.values(descriptor.endpoints)) {
    if (!isSecureEndpointUrl(value)) {
      throw new ProtocolValidationError("Discovery.endpoints");
    }
  }
}

function isSecureEndpointUrl(value: string | URL): boolean {
  let url: URL;
  try {
    url = new URL(value instanceof URL ? value.href : value);
  } catch {
    return false;
  }

  if (url.username.length > 0 || url.password.length > 0) {
    return false;
  }
  return url.protocol === "https:" || (url.protocol === "http:" && isLoopbackHost(url.hostname));
}

function isLoopbackHost(hostname: string): boolean {
  if (hostname === "localhost" || hostname === "[::1]" || hostname === "::1") {
    return true;
  }
  const octets = hostname.split(".");
  return (
    octets.length === 4 &&
    octets[0] === "127" &&
    octets.every((octet) => /^\d{1,3}$/u.test(octet) && Number(octet) <= 255)
  );
}
