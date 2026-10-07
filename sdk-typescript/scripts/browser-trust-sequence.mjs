import { assertSafeBrowserArguments } from "./hub-acceptance-evidence.mjs";

export const SEQUENTIAL_MACOS_TRUST_MODE = "sequential-macos-login-keychain";

const SHA256 = /^[0-9a-f]{64}$/u;

export async function withActionDeadline(action, label, timeoutMs) {
  requireActionTimeout(timeoutMs);
  let timer;
  try {
    return await Promise.race([
      Promise.resolve().then(action),
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${timeoutMs} ms`)),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function requireActionTimeout(timeoutMs) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 60_000) {
    throw new Error("trust action timeout must be an integer between 1 and 60000");
  }
}

export async function runSequentialMacTrustSequence({
  endpoint,
  certificatePath,
  certificateSha256,
  openUntrusted,
  observeUntrusted,
  closeUntrusted,
  importCertificate,
  openTrusted,
  observeTrusted,
  verifyWitness,
  closeTrusted,
  cleanupTrusted,
  removeCertificate,
  verifyCertificateRemoved,
  operationTimeoutMs = 30_000,
  cleanupTimeoutMs = 15_000,
}) {
  if (!SHA256.test(certificateSha256)) throw new Error("certificate SHA-256 is invalid");
  requireActionTimeout(operationTimeoutMs);
  requireActionTimeout(cleanupTimeoutMs);
  const context = { endpoint, certificatePath, certificateSha256 };
  const trustOrder = [];
  let untrustedControl;
  let trustedControl;
  let importStarted = false;
  let importedFingerprint = certificateSha256;
  let imported;
  let primaryError;
  let result;
  const operation = (action, label) => withActionDeadline(action, label, operationTimeoutMs);
  const cleanup = (action, label) => withActionDeadline(action, label, cleanupTimeoutMs);

  try {
    untrustedControl = await operation(openUntrusted, "open untrusted browser");
    if (untrustedControl === undefined || untrustedControl === null) {
      throw new Error("untrusted browser control was not observed");
    }
    trustOrder.push("untrusted_started_without_ca");
    const untrusted = await operation(
      () => observeUntrusted(untrustedControl, context),
      "observe untrusted browser",
    );
    assertPhaseObservation(untrusted, "untrusted", context);
    if (!untrusted.error?.includes("ERR_CERT_AUTHORITY_INVALID")) {
      throw new Error("untrusted browser did not reject with ERR_CERT_AUTHORITY_INVALID");
    }
    trustOrder.push("untrusted_healthz_rejected_ERR_CERT_AUTHORITY_INVALID");
    await operation(() => closeUntrusted(untrustedControl), "close untrusted browser");
    untrustedControl = undefined;
    trustOrder.push("untrusted_closed");

    importStarted = true;
    imported = await operation(() => importCertificate(context), "import certificate");
    assertImportObservation(imported, context);
    importedFingerprint = imported.fingerprint;
    trustOrder.push("fresh_ca_imported");

    trustedControl = await operation(() => openTrusted(imported), "open trusted browser");
    if (trustedControl === undefined || trustedControl === null) {
      throw new Error("trusted browser control was not observed");
    }
    trustOrder.push("trusted_started_with_ca");
    const trusted = await operation(
      () => observeTrusted(trustedControl, context),
      "observe trusted browser",
    );
    assertPhaseObservation(trusted, "trusted", context);
    let witnessVerification;
    if (verifyWitness) {
      witnessVerification = await operation(
        () =>
          verifyWitness({
            ...context,
            context,
            imported,
            untrustedObservation: untrusted,
            trustedObservation: trusted,
            untrustedArguments: untrusted.arguments,
            trustedArguments: trusted.arguments,
          }),
        "verify trust witness",
      );
      trustOrder.push("witness_verified");
    }
    trustOrder.push("trusted_route_observed");

    result = {
      mode: SEQUENTIAL_MACOS_TRUST_MODE,
      certificateSha256,
      importedFingerprint,
      trustOrder,
      imported,
      untrustedObservation: untrusted,
      trustedObservation: trusted,
      witnessVerification,
    };
  } catch (error) {
    primaryError = error;
  } finally {
    const cleanupErrors = [];
    if (trustedControl !== undefined) {
      try {
        await cleanup(() => closeTrusted(trustedControl), "close trusted browser during cleanup");
        trustOrder.push("trusted_closed");
      } catch (error) {
        cleanupErrors.push(error);
      }
    }
    if (untrustedControl !== undefined) {
      try {
        await cleanup(
          () => closeUntrusted(untrustedControl),
          "close untrusted browser during cleanup",
        );
        trustOrder.push("untrusted_closed_during_cleanup");
      } catch (error) {
        cleanupErrors.push(error);
      }
    }
    if (importStarted) {
      const removalContext = { ...context, fingerprint: importedFingerprint };
      if (cleanupTrusted) {
        try {
          const cleaned = await cleanup(
            () => cleanupTrusted(removalContext),
            "trusted browser cleanup",
          );
          assertTrustedBrowserCleanup(cleaned, context);
          trustOrder.push("trusted_browser_cleanup_verified");
        } catch (error) {
          cleanupErrors.push(error);
        }
      }
      try {
        const removed = await cleanup(
          () => removeCertificate(removalContext),
          "remove certificate",
        );
        assertRemovalObservation(removed, importedFingerprint);
        trustOrder.push("fresh_ca_removed");
      } catch (error) {
        cleanupErrors.push(error);
      }
      try {
        const verified = await cleanup(
          () => verifyCertificateRemoved(removalContext),
          "verify certificate removal",
        );
        assertRemovalVerification(verified, importedFingerprint);
        trustOrder.push("fresh_ca_removal_verified");
      } catch (error) {
        cleanupErrors.push(error);
      }
    }
    if (cleanupErrors.length > 0) {
      const cleanupError = new AggregateError(cleanupErrors, "sequential trust cleanup failed");
      if (primaryError instanceof Error) primaryError.cleanupError = cleanupError;
      else primaryError = cleanupError;
    }
  }

  if (primaryError !== undefined) throw primaryError;
  return result;
}

function assertPhaseObservation(observation, phase, context) {
  if (observation?.observed !== true || observation.cdpResult === undefined) {
    throw new Error(`${phase} phase requires an observed CDP result`);
  }
  if (observation.endpoint !== context.endpoint) {
    throw new Error(`${phase} phase endpoint does not match the fresh Hub endpoint`);
  }
  if (observation.certificateSha256 !== context.certificateSha256) {
    throw new Error(`${phase} phase certificate does not match the fresh Hub certificate`);
  }
  assertSafeBrowserArguments(observation.arguments);
}

function assertImportObservation(observation, context) {
  if (observation?.observed !== true) throw new Error("certificate import was not observed");
  if (
    !SHA256.test(observation.fingerprint) ||
    observation.fingerprint !== context.certificateSha256
  ) {
    throw new Error("imported certificate fingerprint does not match the fresh Hub certificate");
  }
  if (
    typeof observation.certificateExportPath !== "string" ||
    observation.certificateExportPath.length === 0
  ) {
    throw new Error("certificate import lacks an exported certificate path");
  }
  if (typeof observation.trustStorePath !== "string" || observation.trustStorePath.length === 0) {
    throw new Error("certificate import lacks a trust-store path");
  }
  if (typeof observation.trustedCdpUrl !== "string" || observation.trustedCdpUrl.length === 0) {
    throw new Error("certificate import lacks a trusted CDP URL");
  }
}

function assertRemovalObservation(observation, fingerprint) {
  if (
    observation?.observed !== true ||
    observation.fingerprint !== fingerprint ||
    observation.removed !== true
  ) {
    throw new Error("certificate removal was not observed for the imported fingerprint");
  }
}

function assertRemovalVerification(observation, fingerprint) {
  if (
    observation?.observed !== true ||
    observation.fingerprint !== fingerprint ||
    observation.present !== false
  ) {
    throw new Error("certificate removal was not verified for the imported fingerprint");
  }
}

function assertTrustedBrowserCleanup(observation, context) {
  if (
    observation?.observed !== true ||
    observation.endpoint !== context.endpoint ||
    observation.certificateSha256 !== context.certificateSha256 ||
    observation.closed !== true ||
    observation.portClosed !== true
  ) {
    throw new Error("trusted browser cleanup was not observed for the fresh endpoint");
  }
}
