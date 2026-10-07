import { mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Ajv2020Module from "ajv/dist/2020.js";
import addFormatsModule from "ajv-formats";
import { describe, expect, it } from "vitest";
import {
  stageDockerPackageContext,
  validateOfficialNodeImageLock,
} from "../../scripts/run-docker-package-gate.mjs";
import {
  historicalArchiveSha256,
  validatePackageAdmissionReceipt,
  type PackageAdmissionReceipt,
} from "../../scripts/package-provenance.mjs";

describe("Docker package input and staging boundaries", () => {
  it("admits the configured trusted image and rejects altered pins or an alternate base", async () => {
    const imageLock = JSON.parse(
      await readFile(new URL("../../tools/node-image-lock.json", import.meta.url), "utf8"),
    );
    const dockerfile = await readFile(new URL("../../Dockerfile", import.meta.url), "utf8");
    expect(validateOfficialNodeImageLock(imageLock, dockerfile)).toBe(imageLock);
    for (const change of [
      { index_digest: `sha256:${"a".repeat(64)}` },
      { linux_arm64_v8_child_digest: `sha256:${"b".repeat(64)}` },
      { official_image: false },
      { repository: "untrusted/node" },
      { tag_observed: "latest" },
    ]) {
      expect(() => validateOfficialNodeImageLock({ ...imageLock, ...change }, dockerfile)).toThrow(
        "provenance lock",
      );
    }
    expect(() =>
      validateOfficialNodeImageLock(imageLock, `${dockerfile}\nFROM untrusted:latest AS extra\n`),
    ).toThrow("provenance lock");
  });

  it("stages exactly the bounded consumer files and creates its own private module manifest", async () => {
    const root = await mkdtemp(join(await realpath(tmpdir()), "teslatlas-docker-stage-test-"));
    try {
      const archivePath = join(root, "synthetic-package.tgz");
      const context = join(root, "context");
      await writeFile(archivePath, "synthetic archive bytes");
      await mkdir(context);
      await stageDockerPackageContext({ context, archivePath });
      expect((await readdir(context)).sort()).toEqual([
        "Dockerfile",
        "consumer",
        "package-smoke.mjs",
        "teslatlas-sdk.tgz",
      ]);
      expect((await readdir(join(context, "consumer"))).sort()).toEqual([
        "app.js",
        "index.html",
        "node.mjs",
        "package.json",
        "serve.mjs",
      ]);
      expect(JSON.parse(await readFile(join(context, "consumer/package.json"), "utf8"))).toEqual({
        name: "teslatlas-docker-package-consumer",
        private: true,
        type: "module",
      });
      expect(await readFile(join(context, "teslatlas-sdk.tgz"), "utf8")).toBe(
        "synthetic archive bytes",
      );
      for (const name of ["node.mjs", "index.html", "app.js", "serve.mjs"]) {
        expect(await readFile(join(context, "consumer", name), "utf8")).toBe(
          await readFile(new URL(`../../examples/hub/${name}`, import.meta.url), "utf8"),
        );
      }
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});

const archive = {
  path: "/synthetic/package.tgz",
  sha256: "a".repeat(64),
  memberCount: 5,
  packageName: "@teslatlas/sdk" as const,
  packageVersion: "2026.10.1",
  packageManager: "npm@12.1.0",
};
const source = {
  root: "/synthetic/source",
  fileCount: 3,
  manifestSha256: "b".repeat(64),
  catalogFileCount: 3,
  catalogManifestSha256: "c".repeat(64),
};
function receipt(role: "candidate" | "predecessor"): PackageAdmissionReceipt {
  return {
    schema_version: 1,
    kind: "teslatlas.sdk-package-admission",
    role,
    state: "independently-reviewed",
    product_version: archive.packageVersion,
    source: {
      repository: "https://github.com/magrathean-uk/teslatlas-sdk-typescript.git",
      commit: "d".repeat(40),
      manifest_sha256: source.manifestSha256,
      file_count: source.fileCount,
    },
    package: {
      name: archive.packageName,
      version: archive.packageVersion,
      archive_sha256: archive.sha256,
      member_count: archive.memberCount,
    },
    toolchain: { node: "26.10.0", npm: "12.1.0" },
    review: {
      verdict: "ACCEPT",
      reviewer_model: role === "candidate" ? "gpt-6.1-sol" : "GPT-5.6 Sol",
      reasoning: role === "candidate" ? "ultra" : "high",
      reviewed_at: "2026-10-07T12:00:00Z",
    },
  };
}

describe("role-specific package review admission", () => {
  it("executes the same current and historical reviewer policy in schema and binding admission", async () => {
    const ajv = new Ajv2020Module.default({ strict: true });
    addFormatsModule.default(ajv);
    const validate = ajv.compile(
      JSON.parse(
        await readFile(
          new URL("../../tools/package-admission.schema.json", import.meta.url),
          "utf8",
        ),
      ),
    );
    const cases = [
      { role: "candidate", model: "gpt-6.1-sol", reasoning: "ultra", valid: true },
      { role: "candidate", model: "GPT-6.1 Sol", reasoning: "ultra", valid: true },
      { role: "candidate", model: "GPT-5.6 Sol", reasoning: "high", valid: false },
      { role: "candidate", model: "gpt-6.1-sol", reasoning: "high", valid: false },
      { role: "predecessor", model: "GPT-5.6 Sol", reasoning: "high", valid: true },
      { role: "predecessor", model: "gpt-6.1-sol", reasoning: "ultra", valid: true },
      { role: "predecessor", model: "GPT-5.6 Sol", reasoning: "ultra", valid: false },
    ] as const;
    for (const entry of cases) {
      const value = receipt(entry.role);
      value.review.reviewer_model = entry.model;
      value.review.reasoning = entry.reasoning;
      expect(validate(value), JSON.stringify(validate.errors)).toBe(entry.valid);
      const admit = () =>
        validatePackageAdmissionReceipt({ role: entry.role, receipt: value, archive, source });
      if (entry.valid) expect(admit()).toBe(value);
      else expect(admit).toThrow("bind accepted");
    }
  });

  it("retains exact candidate toolchain, source, archive and independent-verdict checks", () => {
    for (const mutate of [
      (value: PackageAdmissionReceipt) => {
        value.toolchain.node = "26.7.0";
      },
      (value: PackageAdmissionReceipt) => {
        value.toolchain.npm = "12.0.0";
      },
      (value: PackageAdmissionReceipt) => {
        value.source.manifest_sha256 = "e".repeat(64);
      },
      (value: PackageAdmissionReceipt) => {
        value.package.member_count += 1;
      },
      (value: PackageAdmissionReceipt) => {
        value.review.verdict = "REJECT";
      },
    ]) {
      const value = receipt("candidate");
      mutate(value);
      expect(() =>
        validatePackageAdmissionReceipt({ role: "candidate", receipt: value, archive, source }),
      ).toThrow();
    }
    const value = receipt("candidate");
    value.package.archive_sha256 = historicalArchiveSha256;
    expect(() =>
      validatePackageAdmissionReceipt({
        role: "candidate",
        receipt: value,
        archive: { ...archive, sha256: historicalArchiveSha256 },
        source,
      }),
    ).toThrow("historical accepted archive");
  });
});
