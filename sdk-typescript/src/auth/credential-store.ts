export type MaybePromise<T> = T | Promise<T>;

export interface CredentialStore<TCredential> {
  load(): MaybePromise<TCredential | undefined>;
  save(credential: TCredential): MaybePromise<void>;
  clear(): MaybePromise<void>;
}

export interface AuthorizationContext {
  readonly url: URL;
  readonly method: string;
}

export type AuthorizationProvider = (
  context: AuthorizationContext,
) => MaybePromise<string | undefined>;
