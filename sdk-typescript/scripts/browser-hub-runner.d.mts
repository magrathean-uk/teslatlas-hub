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

export function browserTrustHookPayload<T extends Record<string, unknown>>(
  operation: string,
  payload: T,
): T & { readonly operation: string };

export function browserAndPageCdp(browser: CdpBrowser): Promise<{
  readonly browserCdp: unknown;
  readonly page: unknown;
  readonly networkCdp: unknown;
}>;
