interface CdpContext {
  newPage(): Promise<unknown>;
  newCDPSession(page: unknown): Promise<unknown>;
}

interface CdpBrowser {
  newBrowserCDPSession(): Promise<unknown>;
  contexts(): CdpContext[];
  newContext(): Promise<CdpContext>;
}

export function runBrowserHubAcceptance(): Promise<void>;
export function runTrustHook(
  command: readonly string[],
  payload: Record<string, unknown>,
  options?: { timeoutMs?: number },
): Promise<Record<string, unknown>>;
export function cleanupResources(
  resources: {
    browsers: Set<{ close(): Promise<void> }>;
    server?:
      | {
          listening: boolean;
          close(callback: (error?: Error) => void): unknown;
          closeAllConnections?(): void;
        }
      | undefined;
  },
  timeoutMs?: number,
): Promise<unknown[]>;

export function browserTrustHookPayload<T extends Record<string, unknown>>(
  operation: string,
  payload: T,
): T & { readonly operation: string };

export function browserAndPageCdp(browser: CdpBrowser): Promise<{
  readonly browserCdp: unknown;
  readonly page: unknown;
  readonly networkCdp: unknown;
}>;
