export const HUB_SYNC_SCHEMA: "2.2";
export const HUB_SYNC_SCHEMA_HEADER: "x-teslatlas-supported-schemas";

export function fetchResponseWithDeadline(
  input: RequestInfo | URL,
  init?: RequestInit,
  fetch?: typeof globalThis.fetch,
  timeoutMs?: number,
): Promise<Response>;

export function readBoundedResponseBytes(
  response: Response,
  options: {
    readonly label: string;
    readonly maxBytes: number;
    readonly expectedBytes?: number;
    readonly requireContentLength?: boolean;
    readonly timeoutMs?: number;
  },
): Promise<Uint8Array>;

export function schema22SyncHeaders(
  headers?: Record<string, string>,
): Record<string, string> & { "x-teslatlas-supported-schemas": "2.2" };

export function requestSignedSyncJson(options: {
  readonly endpoint: URL;
  readonly path: string;
  readonly authorization: Record<string, string>;
  readonly signaturePublicKey: string;
  readonly fetch?: typeof globalThis.fetch;
}): Promise<{
  readonly body: unknown;
  readonly bodyBytes: number;
  readonly bodySha256: string;
  readonly cacheControl: string | null;
  readonly signatureBytes: number;
  readonly signaturePresent: true;
  readonly signatureVerified: true;
  readonly status: 200;
}>;

export function validateSchema22Noop(manifest: unknown, noop: unknown): void;
