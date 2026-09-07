export interface PackedSdkWitness {
  readonly packageName: string;
  readonly packageVersion: string;
  readonly entry: string;
  readonly entrySha256: string;
  readonly tarballSha256: string;
  readonly installedContentManifestSha256: string;
  readonly installedMemberCount: number;
}
export function verifyPackedSdk(options: {
  packageRoot: string;
  tarballPath: string;
  expectedTarballSha256: string;
  entryKind: "node" | "browser";
}): Promise<{ entry: string; witness: PackedSdkWitness }>;
export function assertSafeBrowserArguments(arguments_: readonly string[]): void;
export function verifyBrowserTrustWitness(options: {
  witnessPath: string;
  certificatePath: string;
  trustedArguments: readonly string[];
  untrustedArguments: readonly string[];
}): Promise<{ certificateSha256: string; trustedNssDatabase: string }>;
