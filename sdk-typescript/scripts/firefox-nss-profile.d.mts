interface FirefoxLike {
  launchPersistentContext(
    profilePath: string,
    options: {
      readonly firefoxUserPrefs: { readonly "security.enterprise_roots.enabled": false };
      readonly headless: boolean;
      readonly ignoreHTTPSErrors: false;
    },
  ): Promise<{ close(): Promise<void> }>;
}

export const FIREFOX_NSS_CERTUTIL: "/opt/homebrew/opt/nss/bin/certutil";

/** Private script boundary; authority/context are retained only in this process. */
export interface DeferredFirefoxProfileCleanup {
  readonly profilePath: string;
  readonly reason:
    | "close_timed_out"
    | "close_rejected"
    | "settlement_unknown"
    | "remove_failed"
    | "ownership_unknown"
    | "ownership_changed";
  readonly state: "deferred" | "removed";
  retry(): Promise<void>;
}

export class FirefoxNssCleanupError extends AggregateError {
  readonly deferredCleanup: DeferredFirefoxProfileCleanup;
  constructor(errors: readonly unknown[], deferredCleanup: DeferredFirefoxProfileCleanup);
}

export function withFirefoxNssProfile<T>(
  options: {
    readonly certificatePath: string;
    readonly certutilPath?: string;
    readonly firefox: FirefoxLike;
    readonly headless?: boolean;
    readonly temporaryParent?: string;
    readonly closeTimeoutMs?: number;
    readonly certutilTimeoutMs?: number;
  },
  operation: (context: Awaited<ReturnType<FirefoxLike["launchPersistentContext"]>>) => Promise<T>,
  dependencies?: Record<string, unknown>,
): Promise<{
  readonly evidence: {
    readonly certificateNickname: string;
    readonly database: "sql:NSS";
    readonly emptyPassword: true;
    readonly enterpriseRoots: false;
    readonly ignoreHTTPSErrors: false;
    readonly profileCleanup: "removed";
  };
  readonly value: T;
}>;

export function requireOwnerOnlyPath(
  path: string,
  label: string,
  kind: "file" | "directory",
  dependencies?: Record<string, unknown>,
): Promise<string>;

export function requireOwnerOnlyOutputPath(
  path: string,
  label: string,
  dependencies?: Record<string, unknown>,
): Promise<string>;
