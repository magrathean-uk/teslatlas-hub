import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
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
    const root = await mkdtemp(join(tmpdir(), "hub-packed-proof-"));
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

  it("requires an owner-only witness whose exported NSS CA matches the fixture CA", async () => {
    const root = await mkdtemp(join(tmpdir(), "hub-trust-proof-"));
    roots.push(root);
    const certificatePath = join(root, "ca.pem");
    const exportPath = join(root, "nss-export.pem");
    const witnessPath = join(root, "witness.json");
    await writeFile(certificatePath, "fixture-ca");
    await writeFile(exportPath, "fixture-ca");
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
    ).resolves.toEqual({ certificateSha256: digest, trustedNssDatabase: "/private/nss" });
    await writeFile(exportPath, "different-ca");
    await expect(
      verifyBrowserTrustWitness({
        witnessPath,
        certificatePath,
        trustedArguments,
        untrustedArguments,
      }),
    ).rejects.toThrow("does not match fixture CA");
  });
});
