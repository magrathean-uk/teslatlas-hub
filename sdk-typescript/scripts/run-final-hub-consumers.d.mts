import type { CatalogBinding, PackageArchiveBinding } from "./package-provenance.mjs";

export interface FinalHubTarget {
  schema_version: number;
  state: string;
  hub_id: string;
  hub: Record<string, unknown>;
  sdk: Record<string, unknown>;
  descriptor_sha256: string;
  catalog_sha256: string;
}

export function validateFinalTarget(
  target: FinalHubTarget,
  descriptor: Record<string, unknown>,
  packageBinding: Pick<PackageArchiveBinding, "sha256" | "packageVersion" | "memberCount">,
  catalogBinding: Pick<
    CatalogBinding,
    | "sha256"
    | "sourceCommit"
    | "sourceSha256"
    | "packageMemberCount"
    | "admissionReceiptSha256"
    | "admittedHubVersions"
  >,
  descriptorBinding: { sha256: string },
): { hub: Record<string, unknown>; sdk: Record<string, unknown>; hubId: string };
export function runFinalHubConsumers(
  environment?: NodeJS.ProcessEnv,
): Promise<Record<string, unknown>>;
