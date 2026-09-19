import { describe, expect, it } from "vitest";
import { runSequentialMacTrustSequence } from "../../scripts/browser-trust-sequence.mjs";

const endpoint = "https://localhost:18521";
const certificatePath = "/private/fixture/server.pem";
const certificateSha256 = "a".repeat(64);
const safeArguments = ["chromium", "--headless", "--enable-automation"];

function successfulCallbacks(events: string[]) {
  return {
    openUntrusted: async () => {
      events.push("untrusted:start");
      return { name: "untrusted" };
    },
    observeUntrusted: async (
      _control: unknown,
      context: { endpoint: string; certificateSha256: string },
    ) => {
      expect(context).toMatchObject({ endpoint, certificateSha256 });
      events.push("untrusted:observe");
      return {
        observed: true,
        endpoint,
        certificateSha256,
        arguments: safeArguments,
        error: "page.goto failed: net::ERR_CERT_AUTHORITY_INVALID",
        cdpResult: { url: `${endpoint}/healthz`, rejected: true },
      };
    },
    closeUntrusted: async () => {
      events.push("untrusted:close");
    },
    importCertificate: async (context: { certificatePath: string; certificateSha256: string }) => {
      expect(context).toMatchObject({ endpoint, certificatePath, certificateSha256 });
      events.push("certificate:import");
      return {
        observed: true,
        fingerprint: certificateSha256,
        certificateExportPath: "/private/fixture/exported.pem",
        trustStorePath: "/private/fixture/login.keychain-db",
        trustedCdpUrl: "http://127.0.0.1:9227",
      };
    },
    openTrusted: async (imported: { trustedCdpUrl: string }) => {
      expect(imported.trustedCdpUrl).toBe("http://127.0.0.1:9227");
      events.push("trusted:start");
      return { name: "trusted" };
    },
    observeTrusted: async (
      _control: unknown,
      context: { endpoint: string; certificateSha256: string },
    ) => {
      expect(context).toMatchObject({ endpoint, certificateSha256 });
      events.push("trusted:observe");
      return {
        observed: true,
        endpoint,
        certificateSha256,
        arguments: safeArguments,
        cdpResult: { pageOrigin: "http://localhost:4174", hubId: "hub-1" },
      };
    },
    closeTrusted: async () => {
      events.push("trusted:close");
    },
    removeCertificate: async (context: { fingerprint: string }) => {
      expect(context.fingerprint).toBe(certificateSha256);
      events.push("certificate:remove");
      return { observed: true, fingerprint: certificateSha256, removed: true };
    },
    verifyCertificateRemoved: async (context: { fingerprint: string }) => {
      expect(context.fingerprint).toBe(certificateSha256);
      events.push("certificate:verify-removed");
      return { observed: true, fingerprint: certificateSha256, present: false };
    },
  };
}

describe("sequential macOS browser trust", () => {
  it("closes the negative control before import and verifies exact certificate removal", async () => {
    const events: string[] = [];

    const result = await runSequentialMacTrustSequence({
      endpoint,
      certificatePath,
      certificateSha256,
      ...successfulCallbacks(events),
    });

    expect(events).toEqual([
      "untrusted:start",
      "untrusted:observe",
      "untrusted:close",
      "certificate:import",
      "trusted:start",
      "trusted:observe",
      "trusted:close",
      "certificate:remove",
      "certificate:verify-removed",
    ]);
    expect(result).toMatchObject({
      mode: "sequential-macos-login-keychain",
      certificateSha256,
      importedFingerprint: certificateSha256,
      trustOrder: [
        "untrusted_started_without_ca",
        "untrusted_healthz_rejected_ERR_CERT_AUTHORITY_INVALID",
        "untrusted_closed",
        "fresh_ca_imported",
        "trusted_started_with_ca",
        "trusted_route_observed",
        "trusted_closed",
        "fresh_ca_removed",
        "fresh_ca_removal_verified",
      ],
    });
  });

  it("verifies phase witness evidence after both observed CDP phases and before trusted cleanup", async () => {
    const events: string[] = [];
    const callbacks = successfulCallbacks(events);

    const result = await runSequentialMacTrustSequence({
      endpoint,
      certificatePath,
      certificateSha256,
      ...callbacks,
      verifyWitness: async (evidence: {
        endpoint: string;
        certificateSha256: string;
        untrustedArguments: readonly string[];
        trustedArguments: readonly string[];
      }) => {
        events.push("witness:verify");
        expect(evidence).toMatchObject({ endpoint, certificateSha256 });
        expect(evidence.untrustedArguments).toEqual(safeArguments);
        expect(evidence.trustedArguments).toEqual(safeArguments);
        return { certificateSha256, trustMode: "sequential-macos-login-keychain" };
      },
    });

    expect(events.indexOf("witness:verify")).toBeGreaterThan(events.indexOf("trusted:observe"));
    expect(events.indexOf("witness:verify")).toBeLessThan(events.indexOf("trusted:close"));
    expect(result.witnessVerification).toEqual({
      certificateSha256,
      trustMode: "sequential-macos-login-keychain",
    });
  });

  it("stops before certificate import when the negative control does not reject authority", async () => {
    const events: string[] = [];
    const callbacks = successfulCallbacks(events);

    await expect(
      runSequentialMacTrustSequence({
        endpoint,
        certificatePath,
        certificateSha256,
        ...callbacks,
        observeUntrusted: async () => ({
          observed: true,
          endpoint,
          certificateSha256,
          arguments: safeArguments,
          error: "page.goto failed: net::ERR_CERT_DATE_INVALID",
          cdpResult: { rejected: true },
        }),
      }),
    ).rejects.toThrow("ERR_CERT_AUTHORITY_INVALID");

    expect(events).toEqual(["untrusted:start", "untrusted:close"]);
  });

  it("rejects certificate-bypass arguments before import", async () => {
    const events: string[] = [];
    const callbacks = successfulCallbacks(events);

    await expect(
      runSequentialMacTrustSequence({
        endpoint,
        certificatePath,
        certificateSha256,
        ...callbacks,
        observeUntrusted: async () => ({
          observed: true,
          endpoint,
          certificateSha256,
          arguments: ["chromium", "--ignore-certificate-errors"],
          error: "ERR_CERT_AUTHORITY_INVALID",
          cdpResult: { rejected: true },
        }),
      }),
    ).rejects.toThrow("certificate bypass");

    expect(events).toEqual(["untrusted:start", "untrusted:close"]);
  });

  it("rejects an unobserved trusted success and still removes the imported certificate", async () => {
    const events: string[] = [];
    const callbacks = successfulCallbacks(events);

    await expect(
      runSequentialMacTrustSequence({
        endpoint,
        certificatePath,
        certificateSha256,
        ...callbacks,
        observeTrusted: async () => ({
          success: true,
          endpoint,
          certificateSha256,
          arguments: safeArguments,
        }),
      }),
    ).rejects.toThrow("observed CDP result");

    expect(events).toEqual([
      "untrusted:start",
      "untrusted:observe",
      "untrusted:close",
      "certificate:import",
      "trusted:start",
      "trusted:close",
      "certificate:remove",
      "certificate:verify-removed",
    ]);
  });

  it("rejects a trusted observation bound to a different endpoint", async () => {
    const events: string[] = [];
    const callbacks = successfulCallbacks(events);

    await expect(
      runSequentialMacTrustSequence({
        endpoint,
        certificatePath,
        certificateSha256,
        ...callbacks,
        observeTrusted: async () => ({
          observed: true,
          endpoint: "https://localhost:18522",
          certificateSha256,
          arguments: safeArguments,
          cdpResult: { pageOrigin: "http://localhost:4174" },
        }),
      }),
    ).rejects.toThrow("endpoint");

    expect(events).toEqual([
      "untrusted:start",
      "untrusted:observe",
      "untrusted:close",
      "certificate:import",
      "trusted:start",
      "trusted:close",
      "certificate:remove",
      "certificate:verify-removed",
    ]);
  });

  it("runs trusted browser cleanup when import has side effects but returns malformed evidence", async () => {
    const events: string[] = [];
    const callbacks = successfulCallbacks(events);

    await expect(
      runSequentialMacTrustSequence({
        endpoint,
        certificatePath,
        certificateSha256,
        ...callbacks,
        importCertificate: async () => {
          events.push("certificate:import-side-effect");
          return { observed: true, fingerprint: certificateSha256 } as never;
        },
        cleanupTrusted: async (context) => {
          expect(context).toMatchObject({ endpoint, certificateSha256 });
          events.push("trusted:cleanup");
          return {
            observed: true,
            endpoint,
            certificateSha256,
            closed: true,
            portClosed: true,
          };
        },
      }),
    ).rejects.toThrow("exported certificate path");

    expect(events).toEqual([
      "untrusted:start",
      "untrusted:observe",
      "untrusted:close",
      "certificate:import-side-effect",
      "trusted:cleanup",
      "certificate:remove",
      "certificate:verify-removed",
    ]);
  });

  it("runs trusted browser cleanup when CDP attachment fails after import", async () => {
    const events: string[] = [];
    const callbacks = successfulCallbacks(events);

    await expect(
      runSequentialMacTrustSequence({
        endpoint,
        certificatePath,
        certificateSha256,
        ...callbacks,
        openTrusted: async () => {
          events.push("trusted:attach");
          throw new Error("trusted CDP attach failed");
        },
        cleanupTrusted: async () => {
          events.push("trusted:cleanup");
          return {
            observed: true,
            endpoint,
            certificateSha256,
            closed: true,
            portClosed: true,
          };
        },
      }),
    ).rejects.toThrow("trusted CDP attach failed");

    expect(events).toEqual([
      "untrusted:start",
      "untrusted:observe",
      "untrusted:close",
      "certificate:import",
      "trusted:attach",
      "trusted:cleanup",
      "certificate:remove",
      "certificate:verify-removed",
    ]);
  });
});
