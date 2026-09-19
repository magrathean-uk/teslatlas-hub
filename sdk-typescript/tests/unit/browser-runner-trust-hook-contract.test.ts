import { describe, expect, it } from "vitest";
import { browserTrustHookPayload } from "../../scripts/browser-hub-runner.mjs";

describe("browser runner trust-hook contract", () => {
  it("uses the reviewed hook keys for import, browser cleanup, removal, and removal verification", () => {
    const binding = {
      endpoint: "https://localhost:18537",
      certificatePath: "/private/hub/server.pem",
      certificateSha256: "a".repeat(64),
    };
    const trustedCdpUrl = "http://127.0.0.1:9248";
    const expectedCertificateExportPath = "/private/control/imported-hub-ca.pem";
    const fingerprint = "b".repeat(64);

    expect(
      browserTrustHookPayload("import", {
        ...binding,
        trustedCdpUrl,
        expectedCertificateExportPath,
      }),
    ).toEqual({
      operation: "import",
      ...binding,
      trustedCdpUrl,
      expectedCertificateExportPath,
    });

    expect(
      browserTrustHookPayload("trusted-browser-cleanup", {
        ...binding,
        fingerprint,
        trustedCdpUrl,
        expectedCertificateExportPath,
        witnessPath: "/private/control/browser-trust-witness.json",
      }),
    ).toEqual({
      operation: "trusted-browser-cleanup",
      ...binding,
      fingerprint,
      trustedCdpUrl,
      expectedCertificateExportPath,
      witnessPath: "/private/control/browser-trust-witness.json",
    });

    for (const operation of ["remove", "verify-removal"] as const) {
      expect(browserTrustHookPayload(operation, { ...binding, fingerprint })).toEqual({
        operation,
        ...binding,
        fingerprint,
      });
    }
  });
});
