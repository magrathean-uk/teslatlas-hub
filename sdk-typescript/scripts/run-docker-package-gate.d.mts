export interface DockerGateOptions {
  tarball: string;
  sha256: string;
  candidateReceipt: string;
  candidateReceiptSha256: string;
  sourceExport: string;
  catalog: string;
  catalogSha256: string;
}

export function validateDockerGateInputs(
  options: DockerGateOptions,
): Promise<Record<string, unknown>>;
export function runDockerPackageGate(options: DockerGateOptions): Promise<Record<string, unknown>>;
export function cleanupDockerArtifacts(options: {
  tag: string;
  context: string;
  removeImage?: (tag: string) => void;
  removeContext?: (path: string) => Promise<void>;
}): Promise<void>;
