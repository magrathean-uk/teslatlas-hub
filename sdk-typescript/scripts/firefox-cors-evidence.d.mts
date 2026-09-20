interface BrowserRequestLike {
  method(): string;
  url(): string;
}

interface FirefoxPageLike {
  close(): Promise<void>;
  evaluate(expression: () => unknown): Promise<unknown>;
  goto(url: string, options: { waitUntil: "load" }): Promise<unknown>;
  waitForFunction(
    expression: () => boolean,
    argument: null,
    options: { timeout: number },
  ): Promise<unknown>;
}

interface FirefoxContextLike {
  browser(): { version(): string };
  newPage(): Promise<FirefoxPageLike>;
  off(event: "request", listener: (request: BrowserRequestLike) => void): void;
  on(event: "request", listener: (request: BrowserRequestLike) => void): void;
}

export function observeFirefoxCorsReads(
  context: FirefoxContextLike,
  appOrigin: URL,
  hubEndpoint: string,
): Promise<
  Record<string, unknown> & {
    readonly cors: {
      readonly browserEnforced: true;
      readonly hubRequestsObserved: number;
      readonly preflightVisibility: "not-exposed-by-playwright-firefox-request-events";
    };
    readonly runtime: string;
  }
>;
