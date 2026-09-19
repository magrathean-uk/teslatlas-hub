export const SEQUENTIAL_MACOS_TRUST_MODE: "sequential-macos-login-keychain";
export interface TrustPhaseContext {
  readonly endpoint: string;
  readonly certificatePath: string;
  readonly certificateSha256: string;
}
export interface BrowserTrustPhaseObservation {
  readonly observed?: boolean;
  readonly endpoint: string;
  readonly certificateSha256: string;
  readonly arguments: readonly string[];
  readonly error?: string;
  readonly cdpResult?: unknown;
}
export interface CertificateImportObservation {
  readonly observed: boolean;
  readonly fingerprint: string;
  readonly certificateExportPath: string;
  readonly trustStorePath: string;
  readonly trustedCdpUrl: string;
}
export interface CertificateRemovalObservation {
  readonly observed: boolean;
  readonly fingerprint: string;
  readonly removed?: boolean;
  readonly present?: boolean;
}
export interface TrustedBrowserCleanupObservation {
  readonly observed: boolean;
  readonly endpoint: string;
  readonly certificateSha256: string;
  readonly trustedCdpUrl?: string;
  readonly closed: boolean;
  readonly portClosed: boolean;
}
export function runSequentialMacTrustSequence(options: {
  readonly endpoint: string;
  readonly certificatePath: string;
  readonly certificateSha256: string;
  readonly openUntrusted: () => Promise<unknown>;
  readonly observeUntrusted: (
    control: unknown,
    context: TrustPhaseContext,
  ) => Promise<BrowserTrustPhaseObservation>;
  readonly closeUntrusted: (control: unknown) => Promise<void>;
  readonly importCertificate: (context: TrustPhaseContext) => Promise<CertificateImportObservation>;
  readonly openTrusted: (imported: CertificateImportObservation) => Promise<unknown>;
  readonly observeTrusted: (
    control: unknown,
    context: TrustPhaseContext,
  ) => Promise<BrowserTrustPhaseObservation>;
  readonly verifyWitness?: (evidence: {
    readonly endpoint: string;
    readonly certificatePath: string;
    readonly certificateSha256: string;
    readonly context: TrustPhaseContext;
    readonly imported: CertificateImportObservation;
    readonly untrustedObservation: BrowserTrustPhaseObservation;
    readonly trustedObservation: BrowserTrustPhaseObservation;
    readonly untrustedArguments: readonly string[];
    readonly trustedArguments: readonly string[];
  }) => Promise<unknown>;
  readonly closeTrusted: (control: unknown) => Promise<void>;
  readonly cleanupTrusted?: (
    context: TrustPhaseContext & { readonly fingerprint: string },
  ) => Promise<TrustedBrowserCleanupObservation>;
  readonly removeCertificate: (
    context: TrustPhaseContext & { readonly fingerprint: string },
  ) => Promise<CertificateRemovalObservation>;
  readonly verifyCertificateRemoved: (
    context: TrustPhaseContext & { readonly fingerprint: string },
  ) => Promise<CertificateRemovalObservation>;
}): Promise<{
  readonly mode: "sequential-macos-login-keychain";
  readonly certificateSha256: string;
  readonly importedFingerprint: string;
  readonly trustOrder: readonly string[];
  readonly imported: CertificateImportObservation;
  readonly untrustedObservation: BrowserTrustPhaseObservation;
  readonly trustedObservation: BrowserTrustPhaseObservation;
  readonly witnessVerification?: unknown;
}>;
