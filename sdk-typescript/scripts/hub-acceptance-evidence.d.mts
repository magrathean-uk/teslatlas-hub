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
  entryKind: "node";
}): Promise<{ entry: string; witness: PackedSdkWitness }>;
