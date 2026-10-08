import type {
  CatalogBinding,
  PackageArchiveBinding,
  ReviewedPackageBinding,
} from "./package-provenance.mjs";

export interface LifecycleOptions {
  candidate: string;
  candidateSha256: string;
  candidateReceipt: string;
  candidateReceiptSha256: string;
  candidateSourceExport: string;
  predecessor: string;
  predecessorSha256: string;
  predecessorReceipt: string;
  predecessorReceiptSha256: string;
  predecessorSourceExport: string;
  catalog: string;
  catalogSha256: string;
  receipt?: string;
}

export function validateLifecycleInputs(options: LifecycleOptions): Promise<{
  candidate: PackageArchiveBinding;
  predecessor: PackageArchiveBinding;
  candidateAdmission: ReviewedPackageBinding;
  predecessorAdmission: ReviewedPackageBinding;
  candidateCatalog: CatalogBinding;
  predecessorCatalog: CatalogBinding;
}>;
export function runLifecycle(options: LifecycleOptions): Promise<Record<string, unknown>>;
export function parseLifecycleArguments(values: string[]): LifecycleOptions;
