import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmod,
  mkdir,
  mkdtemp,
  readFile,
  realpath,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
  assertSafeBrowserArguments,
  verifyBrowserTrustWitness,
  verifyPackedSdk,
} from "../../scripts/hub-acceptance-evidence.mjs";

const roots: string[] = [];
afterEach(async () => Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true }))));

describe("Hub acceptance evidence admission", () => {
  it("binds an installed entry to the expected tarball bytes and rejects substitution", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "hub-packed-proof-")));
    roots.push(root);
    const packed = join(root, "package");
    const installed = join(root, "node_modules/@teslatlas/sdk");
    await mkdir(join(packed, "dist"), { recursive: true });
    await mkdir(join(installed, "dist"), { recursive: true });
    const metadata = JSON.stringify({ name: "@teslatlas/sdk", version: "2026.36.2" });
    await writeFile(join(packed, "package.json"), metadata);
    await mkdir(join(packed, "dist/hub"), { recursive: true });
    await mkdir(join(installed, "dist/hub"), { recursive: true });
    await writeFile(join(packed, "dist/node.js"), 'export * from "./hub/client.js";\n');
    await writeFile(join(packed, "dist/hub/client.js"), "export const packed = true;\n");
    await writeFile(join(installed, "package.json"), metadata);
    await writeFile(join(installed, "dist/node.js"), 'export * from "./hub/client.js";\n');
    await writeFile(join(installed, "dist/hub/client.js"), "export const packed = true;\n");
    const tarball = join(root, "sdk.tgz");
    execFileSync("tar", ["-czf", tarball, "package"], { cwd: root });
    const expectedTarballSha256 = createHash("sha256")
      .update(await readFile(tarball))
      .digest("hex");

    await expect(
      verifyPackedSdk({
        packageRoot: installed,
        tarballPath: tarball,
        expectedTarballSha256,
        entryKind: "node",
      }),
    ).resolves.toMatchObject({
      witness: {
        packageVersion: "2026.36.2",
        tarballSha256: expectedTarballSha256,
        installedMemberCount: 3,
        installedContentManifestSha256: expect.stringMatching(/^[0-9a-f]{64}$/),
      },
    });
    await writeFile(join(installed, "dist/hub/client.js"), "export const substituted = true;\n");
    await expect(
      verifyPackedSdk({
        packageRoot: installed,
        tarballPath: tarball,
        expectedTarballSha256,
        entryKind: "node",
      }),
    ).rejects.toThrow("does not match tarball");
  });

  it("rejects live browser certificate bypass arguments", () => {
    expect(() => assertSafeBrowserArguments(["chromium", "--ignore-certificate-errors"])).toThrow(
      "certificate bypass",
    );
    expect(() =>
      assertSafeBrowserArguments(["chromium", "--ignore-certificate-errors-spki-list"]),
    ).toThrow("certificate bypass");
    expect(() =>
      assertSafeBrowserArguments([
        "chromium",
        "--ignore-certificate-errors-spki-list=sha256/deadbeef",
      ]),
    ).toThrow("certificate bypass");
    expect(() =>
      assertSafeBrowserArguments(["chromium", "--headless", "--enable-automation"]),
    ).not.toThrow();
  });

  it("requires an owner-only witness whose exported certificate matches the fixture CA", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "hub-trust-proof-")));
    roots.push(root);
    const certificatePath = join(root, "ca.pem");
    const exportPath = join(root, "nss-export.pem");
    const witnessPath = join(root, "witness.json");
    await writeFile(certificatePath, "fixture-ca");
    await writeFile(exportPath, "fixture-ca");
    await chmod(exportPath, 0o600);
    const digest = createHash("sha256").update("fixture-ca").digest("hex");
    const trustedArguments = ["chromium", "--headless", "--enable-automation"];
    const untrustedArguments = ["chromium", "--headless", "--enable-automation"];
    await writeFile(
      witnessPath,
      JSON.stringify({
        schemaVersion: 1,
        certificateSha256: digest,
        trusted: {
          arguments: trustedArguments,
          nssCaExportPath: exportPath,
          nssDatabase: "/private/nss",
        },
        untrusted: { arguments: untrustedArguments, nssDatabase: "/private/empty-nss" },
      }),
    );
    await chmod(witnessPath, 0o600);

    await expect(
      verifyBrowserTrustWitness({
        witnessPath,
        certificatePath,
        trustedArguments,
        untrustedArguments,
      }),
    ).resolves.toEqual({
      certificateSha256: digest,
      trustedTrustStore: "/private/nss",
      untrustedTrustStore: "/private/empty-nss",
      certificateExportPath: exportPath,
      trustMode: "legacy-simultaneous",
    });
    await writeFile(exportPath, "different-ca");
    await expect(
      verifyBrowserTrustWitness({
        witnessPath,
        certificatePath,
        trustedArguments,
        untrustedArguments,
      }),
    ).rejects.toThrow("does not match fixture certificate");
  });

  it("validates neutral sequential witness fields for exported certificate and keychain evidence", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "hub-sequential-trust-proof-")));
    roots.push(root);
    const certificatePath = join(root, "ca.pem");
    const exportPath = join(root, "certificate-export.pem");
    const witnessPath = join(root, "witness.json");
    await writeFile(certificatePath, "fixture-ca");
    await writeFile(exportPath, "fixture-ca");
    await chmod(exportPath, 0o600);
    const digest = createHash("sha256").update("fixture-ca").digest("hex");
    const arguments_ = ["chromium", "--headless", "--enable-automation"];
    await writeFile(
      witnessPath,
      JSON.stringify({
        schemaVersion: 2,
        mode: "sequential-macos-login-keychain",
        certificateSha256: digest,
        phaseOrder: [
          "untrusted_started_without_ca",
          "untrusted_healthz_rejected_ERR_CERT_AUTHORITY_INVALID",
          "untrusted_closed",
          "fresh_ca_imported",
          "trusted_started_with_ca",
          "trusted_route_observed",
        ],
        untrustedBeforeImport: {
          arguments: arguments_,
          error: "net::ERR_CERT_AUTHORITY_INVALID",
          cdpResult: { product: "Chromium" },
          trustStorePath: "/private/untrusted-login-keychain",
        },
        trustedAfterImport: {
          arguments: arguments_,
          cdpResult: { result: { value: { ok: true } } },
          certificateExportPath: exportPath,
          trustStorePath: "/private/trusted-login-keychain",
        },
      }),
    );
    await chmod(witnessPath, 0o600);

    await expect(
      verifyBrowserTrustWitness({
        mode: "sequential-macos-login-keychain",
        witnessPath,
        certificatePath,
        expectedCertificateExportPath: exportPath,
        trustedArguments: arguments_,
        untrustedArguments: arguments_,
      }),
    ).resolves.toEqual({
      certificateSha256: digest,
      trustedTrustStore: "/private/trusted-login-keychain",
      untrustedTrustStore: "/private/untrusted-login-keychain",
      certificateExportPath: exportPath,
      trustMode: "sequential-macos-login-keychain",
    });
  });

  it("rejects export substitution, duplicate bytes, symlinks and non-owner-only permissions", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "hub-export-admission-proof-")));
    roots.push(root);
    const certificatePath = join(root, "ca.pem");
    const exportPath = join(root, "certificate-export.pem");
    const duplicatePath = join(root, "duplicate-export.pem");
    const witnessPath = join(root, "witness.json");
    const arguments_ = ["chromium", "--headless", "--enable-automation"];
    await writeFile(certificatePath, "fixture-ca");
    await chmod(certificatePath, 0o600);
    await writeFile(exportPath, "fixture-ca");
    await chmod(exportPath, 0o600);
    const digest = createHash("sha256").update("fixture-ca").digest("hex");
    await writeFile(
      witnessPath,
      JSON.stringify({
        schemaVersion: 2,
        mode: "sequential-macos-login-keychain",
        certificateSha256: digest,
        phaseOrder: [
          "untrusted_started_without_ca",
          "untrusted_healthz_rejected_ERR_CERT_AUTHORITY_INVALID",
          "untrusted_closed",
          "fresh_ca_imported",
          "trusted_started_with_ca",
          "trusted_route_observed",
        ],
        untrustedBeforeImport: {
          arguments: arguments_,
          error: "net::ERR_CERT_AUTHORITY_INVALID",
          cdpResult: { product: "Chromium" },
          trustStorePath: join(root, "untrusted-keychain"),
        },
        trustedAfterImport: {
          arguments: arguments_,
          cdpResult: { result: { value: { ok: true } } },
          certificateExportPath: exportPath,
          trustStorePath: join(root, "login.keychain-db"),
        },
      }),
    );
    await chmod(witnessPath, 0o600);
    const options = {
      mode: "sequential-macos-login-keychain" as const,
      witnessPath,
      certificatePath,
      expectedCertificateExportPath: exportPath,
      trustedArguments: arguments_,
      untrustedArguments: arguments_,
    };

    await expect(verifyBrowserTrustWitness(options)).resolves.toBeDefined();

    const witnessBytes = await readFile(witnessPath);
    const realWitnessPath = join(root, "witness-real.json");
    await writeFile(realWitnessPath, witnessBytes);
    await chmod(realWitnessPath, 0o600);
    await rm(witnessPath);
    await symlink(realWitnessPath, witnessPath);
    await expect(verifyBrowserTrustWitness(options)).rejects.toThrow("canonical");
    await rm(witnessPath);
    await writeFile(witnessPath, witnessBytes);
    await chmod(witnessPath, 0o644);
    await expect(verifyBrowserTrustWitness(options)).rejects.toThrow("owner-only");
    await chmod(witnessPath, 0o600);

    await rm(exportPath);
    await symlink(certificatePath, exportPath);
    await expect(verifyBrowserTrustWitness(options)).rejects.toThrow("canonical");

    await rm(exportPath);
    await writeFile(exportPath, "fixture-ca");
    await chmod(exportPath, 0o644);
    await expect(verifyBrowserTrustWitness(options)).rejects.toThrow("owner-only");

    await chmod(exportPath, 0o600);
    await writeFile(duplicatePath, "fixture-ca");
    await chmod(duplicatePath, 0o600);
    const witnessWithDuplicate = JSON.parse(await readFile(witnessPath, "utf8"));
    witnessWithDuplicate.trustedAfterImport.certificateExportPath = duplicatePath;
    await writeFile(witnessPath, JSON.stringify(witnessWithDuplicate));
    await expect(verifyBrowserTrustWitness(options)).rejects.toThrow("expected fresh export path");

    await writeFile(
      witnessPath,
      JSON.stringify({
        schemaVersion: 2,
        mode: "sequential-macos-login-keychain",
        certificateSha256: digest,
        phaseOrder: [
          "untrusted_started_without_ca",
          "untrusted_healthz_rejected_ERR_CERT_AUTHORITY_INVALID",
          "untrusted_closed",
          "fresh_ca_imported",
          "trusted_started_with_ca",
          "trusted_route_observed",
        ],
        untrustedBeforeImport: {
          arguments: arguments_,
          error: "net::ERR_CERT_AUTHORITY_INVALID",
          cdpResult: { product: "Chromium" },
          trustStorePath: join(root, "untrusted-keychain"),
        },
        trustedAfterImport: {
          arguments: arguments_,
          cdpResult: { result: { value: { ok: true } } },
          certificateExportPath: certificatePath,
          trustStorePath: join(root, "login.keychain-db"),
        },
      }),
    );
    await chmod(witnessPath, 0o600);
    await expect(
      verifyBrowserTrustWitness({ ...options, expectedCertificateExportPath: certificatePath }),
    ).rejects.toThrow("distinct");
  });
});
