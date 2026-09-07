export function candidateSourceIdentity(
  repository: string,
  baseCommit: string,
  entries: Readonly<Record<string, string>>,
): {
  kind: "local-content";
  repository: string;
  baseCommit: string;
  contentSha256: string;
};

export function filesForProfile(
  entries: Readonly<Record<string, string>>,
  profile: "richer" | "currentHub",
): Record<string, string>;

export function sha256DigestMap(entries: Readonly<Record<string, string>>): string;
