export interface PackageArchiveBinding {
  path: string;
  sha256: string;
  memberCount: number;
  packageName: "@teslatlas/sdk";
  packageVersion: string;
  packageManager: string | null;
}

export interface SourceExportBinding {
  root: string;
  fileCount: number;
  manifestSha256: string;
  catalogFileCount: number;
  catalogManifestSha256: string;
}

export interface ReviewedPackageBinding {
  role: "candidate" | "predecessor";
  receiptPath: string;
  receiptSha256: string;
  receipt: PackageAdmissionReceipt;
  source: SourceExportBinding;
}

export interface PackageAdmissionReceipt {
  schema_version: number;
  kind: string;
  role: string;
  state: string;
  product_version: string;
  source: { repository: string; commit: string; manifest_sha256: string; file_count: number };
  package: { name: string; version: string; archive_sha256: string; member_count: number };
  toolchain: { node: string; npm: string };
  review: { verdict: string; reviewer_model: string; reasoning: string; reviewed_at: string };
}

export interface CatalogBinding {
  path: string;
  sha256: string;
  publicationStatus: "local-unpublished" | "published";
  admittedHubVersions: string[];
  sourceCommit: string;
  sourceSha256: string;
  packageSha256: string;
  packageMemberCount: number;
  admissionReceiptSha256: string;
  profile: { id: string; revision: string; sha256: string };
}

export function requireSha256(value: string, label: string): string;
export function requireCommit(value: string, label: string): string;
export function canonicalRegularFile(filePath: string, label: string): Promise<string>;
export function canonicalDirectory(filePath: string, label: string): Promise<string>;
export function sha256File(filePath: string): Promise<string>;
export function parseStrictJson(source: string, label?: string): unknown;
export function readStrictJsonFile(
  filePath: string,
  label: string,
  expectedSha256?: string,
): Promise<{ path: string; sha256: string; value: unknown }>;
export function inspectPackageArchive(
  filePath: string,
  expectedSha256: string,
  label?: string,
): Promise<PackageArchiveBinding>;
export function sourceExportManifest(
  sourceExport: string,
  label?: string,
): Promise<SourceExportBinding>;
export function validateReviewedPackageBinding(options: {
  role: "candidate" | "predecessor";
  receiptPath: string;
  receiptSha256: string;
  archive: PackageArchiveBinding;
  sourceExport: string;
}): Promise<ReviewedPackageBinding>;
export function readCatalogBinding(options: {
  catalogPath: string;
  catalogSha256: string;
  reviewedPackage: ReviewedPackageBinding;
}): Promise<CatalogBinding>;
export function compareProductVersions(predecessor: string, candidate: string): -1 | 0 | 1;
export const historicalArchiveSha256: string;
