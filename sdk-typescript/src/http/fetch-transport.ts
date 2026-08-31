import type { AuthorizationProvider } from "../auth/credential-store.js";
import { containsControlCharacters, TeslatlasError, TransportError } from "../core/errors.js";
import type { EntityTag } from "../core/opaque-values.js";
import { applyIfNoneMatch } from "./conditional.js";

export { TransportError } from "../core/errors.js";

export type FetchImplementation = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

export interface FetchTransportOptions {
  readonly baseUrl: string | URL;
  readonly fetch?: FetchImplementation;
  readonly authorization?: AuthorizationProvider;
}

export interface ProtocolRequestInit extends Omit<RequestInit, "headers"> {
  readonly headers?: HeadersInit;
  readonly ifNoneMatch?: EntityTag;
  /** @internal */
  readonly onDispatch?: () => void;
}

export class InvalidBaseUrlError extends TeslatlasError<"invalid_base_url"> {
  constructor() {
    super("Base URL must be an HTTP or HTTPS URL without embedded credentials", {
      code: "invalid_base_url",
    });
  }
}

export class InvalidRequestPathError extends TeslatlasError<"invalid_request_path"> {
  constructor() {
    super("Protocol request target must be a root-relative path", {
      code: "invalid_request_path",
    });
  }
}

export class ReservedAuthorizationHeaderError extends TeslatlasError<"reserved_authorization_header"> {
  constructor() {
    super("Authorization header must come from the caller authorization provider", {
      code: "reserved_authorization_header",
    });
  }
}

export class InvalidAuthorizationValueError extends TeslatlasError<"invalid_authorization_value"> {
  constructor() {
    super("Authorization value must be non-empty and contain no control characters", {
      code: "invalid_authorization_value",
    });
  }
}

export class MissingFetchError extends TeslatlasError<"missing_fetch"> {
  constructor() {
    super("A Fetch API implementation is required", { code: "missing_fetch" });
  }
}

export class FetchTransport {
  readonly #baseUrl: URL;
  readonly #fetch: FetchImplementation;
  readonly #authorization: AuthorizationProvider | undefined;

  constructor(options: FetchTransportOptions) {
    this.#baseUrl = parseBaseUrl(options.baseUrl);
    this.#fetch = resolveFetch(options.fetch);
    this.#authorization = options.authorization;
  }

  async request(path: string, init: ProtocolRequestInit = {}): Promise<Response> {
    const url = resolveRequestUrl(this.#baseUrl, path);
    const {
      headers: inputHeaders,
      ifNoneMatch,
      method: requestedMethod,
      onDispatch,
      ...remainingInit
    } = init;
    const method = (requestedMethod ?? "GET").toUpperCase();
    let headers = new Headers(inputHeaders);

    if (headers.has("Authorization")) {
      throw new ReservedAuthorizationHeaderError();
    }
    if (ifNoneMatch !== undefined) {
      headers = applyIfNoneMatch(headers, ifNoneMatch);
    }

    try {
      const authorization = await this.#authorization?.({ url: new URL(url.href), method });
      if (authorization !== undefined) {
        if (authorization.length === 0 || containsControlCharacters(authorization)) {
          throw new InvalidAuthorizationValueError();
        }
        headers.set("Authorization", authorization);
      }

      onDispatch?.();
      return await this.#fetch(url, {
        ...remainingInit,
        method,
        headers,
      });
    } catch (error) {
      if (error instanceof TeslatlasError) {
        throw error;
      }
      if (init.signal?.aborted === true) {
        throw init.signal.reason ?? error;
      }
      if (isAbortError(error)) {
        throw error;
      }
      throw new TransportError();
    }
  }
}

function parseBaseUrl(value: string | URL): URL {
  let url: URL;
  try {
    url = new URL(value instanceof URL ? value.href : value);
  } catch {
    throw new InvalidBaseUrlError();
  }

  if (
    (url.protocol !== "http:" && url.protocol !== "https:") ||
    url.username.length > 0 ||
    url.password.length > 0
  ) {
    throw new InvalidBaseUrlError();
  }
  return url;
}

function resolveFetch(injected: FetchImplementation | undefined): FetchImplementation {
  if (injected !== undefined) {
    return injected;
  }
  if (typeof globalThis.fetch !== "function") {
    throw new MissingFetchError();
  }
  return globalThis.fetch.bind(globalThis);
}

function resolveRequestUrl(baseUrl: URL, path: string): URL {
  if (!path.startsWith("/") || path.startsWith("//") || containsControlCharacters(path)) {
    throw new InvalidRequestPathError();
  }
  const url = new URL(path, baseUrl);
  if (url.origin !== baseUrl.origin) {
    throw new InvalidRequestPathError();
  }
  return url;
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}
