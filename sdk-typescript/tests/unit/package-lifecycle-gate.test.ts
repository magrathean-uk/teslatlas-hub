import { mkdir, mkdtemp, readdir, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gzipSync } from "node:zlib";
import { afterEach, describe, expect, it } from "vitest";
import { validateLifecycleInputs } from "../../scripts/package-lifecycle-gate.mjs";
import {
  compareProductVersions,
  historicalArchiveSha256,
  inspectPackageArchive,
  parseStrictJson,
  readCatalogBinding,
  sha256File,
  sourceExportManifest,
  validateReviewedPackageBinding,
} from "../../scripts/package-provenance.mjs";
import { cleanupDockerArtifacts } from "../../scripts/run-docker-package-gate.mjs";
import { validateFinalTarget } from "../../scripts/run-final-hub-consumers.mjs";

const temporaryRoots: string[] = [];
const profile = {
  id: "hub-http-v1",
  revision: "1.0.0",
  sha256: "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926",
};
const edgeProfile = {
  id: "edge-delivery-v2",
  revision: "2.0.0",
  sha256: "e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224",
};

afterEach(async () => {
  await Promise.all(
    temporaryRoots.splice(0).map((path) => rm(path, { recursive: true, force: true })),
  );
});

describe("package lifecycle provenance", () => {
  it("orders calendar versions and detects duplicate JSON keys", () => {
    expect(compareProductVersions("2026.36.1", "2026.36.2")).toBe(-1);
    expect(compareProductVersions("2026.36.2", "2026.36.2")).toBe(0);
    expect(() => parseStrictJson('{"state":"accepted","state":"draft"}', "receipt")).toThrow(
      'receipt contains duplicate key "state"',
    );
  });

  it.each([
    ["duplicate member", [{ name: "package/package.json", type: "0", mode: 0o644 }]],
    ["symbolic link", [{ name: "package/link", type: "2", mode: 0o777 }]],
    ["hard link", [{ name: "package/link", type: "1", mode: 0o644 }]],
    ["special entry", [{ name: "package/fifo", type: "6", mode: 0o644 }]],
    ["unsafe path", [{ name: "package/../escape", type: "0", mode: 0o644 }]],
    ["unsafe mode", [{ name: "package/world-write", type: "0", mode: 0o666 }]],
  ])("rejects %s before npm or Docker", async (_label, additions) => {
    const root = await temporaryRoot();
    const archive = await packageArchive(root, "2026.36.2", additions);
    await expect(inspectPackageArchive(archive.path, archive.sha256)).rejects.toThrow();
  });

  it("rejects changed source-export bytes after an independent receipt was issued", async () => {
    const root = await temporaryRoot();
    const source = await sourceExport(root, "candidate-source", "original\n");
    const archive = await admittedArchive(root, "2026.36.2");
    const admission = await admissionReceipt(root, "candidate", source, archive);
    await writeFile(join(source.root, "README.md"), "dirty replacement\n");

    await expect(
      validateReviewedPackageBinding({
        role: "candidate",
        receiptPath: admission.path,
        receiptSha256: admission.sha256,
        archive,
        sourceExport: source.root,
      }),
    ).rejects.toThrow("candidate receipt does not bind accepted source/package/toolchain evidence");
  });

  it("matches the Hub bootstrap exclusions, ordering and Unicode JSON identity", async () => {
    const root = await temporaryRoot();
    const source = await sourceExport(root, "candidate-source", "candidate\n");
    await writeFile(join(source.root, "AGENTS.md"), "operator instructions\n", { mode: 0o600 });
    await mkdir(join(source.root, ".build"), { mode: 0o700 });
    await writeFile(join(source.root, ".build", "ignored.txt"), "ignored\n", { mode: 0o600 });
    await mkdir(join(source.root, "coverage"), { mode: 0o700 });
    await writeFile(join(source.root, "coverage", "ignored.txt"), "ignored\n", { mode: 0o600 });
    await mkdir(join(source.root, "a"), { mode: 0o700 });
    await writeFile(join(source.root, "a", "β.txt"), "beta\n", { mode: 0o600 });
    await writeFile(join(source.root, "z.txt"), "zed\n", { mode: 0o600 });
    await writeFile(join(source.root, "😀.txt"), "smile\n", { mode: 0o600 });
    const manifests = await sourceExportManifest(source.root);

    expect(manifests.fileCount).toBe(7);
    expect(manifests.catalogFileCount).toBe(4);
    expect(manifests.catalogManifestSha256).toBe(
      "23650d501597e4f0663e1d7c4ecd01771c6af0983c3ba5ce9aff939b3f2540b7",
    );
  });

  it("rejects a source-file symlink before either manifest is admitted", async () => {
    const root = await temporaryRoot();
    const source = await sourceExport(root, "candidate-source", "candidate\n");
    await symlink("README.md", join(source.root, "linked-readme"));

    await expect(sourceExportManifest(source.root)).rejects.toThrow(
      "source export contains a link or special entry",
    );
  });

  it("rejects directory membership added after its initial read", async () => {
    const root = await temporaryRoot();
    const source = await sourceExport(root, "candidate-source", "candidate\n");
    const inspectWithHook = sourceExportManifest as unknown as (
      path: string,
      label: string,
      hooks: { afterDirectoryRead(directory: string): Promise<void> },
    ) => ReturnType<typeof sourceExportManifest>;
    let injected = false;

    await expect(
      inspectWithHook(source.root, "source export", {
        async afterDirectoryRead(directory) {
          if (!injected && directory === source.root) {
            injected = true;
            await writeFile(join(source.root, "late-addition.txt"), "late\n", { mode: 0o600 });
          }
        },
      }),
    ).rejects.toThrow("source export changed while it was inspected");
  });

  it("explicitly rejects the historical d94 archive as the changed candidate", async () => {
    const root = await temporaryRoot();
    const source = await sourceExport(root, "candidate-source", "candidate\n");
    const archive = {
      path: join(root, "teslatlas-sdk-2026.36.2.tgz"),
      sha256: historicalArchiveSha256,
      memberCount: 83,
      packageName: "@teslatlas/sdk" as const,
      packageVersion: "2026.36.2",
      packageManager: "npm@11.19.0",
    };
    const admission = await admissionReceipt(root, "candidate", source, archive);

    await expect(
      validateReviewedPackageBinding({
        role: "candidate",
        receiptPath: admission.path,
        receiptSha256: admission.sha256,
        archive,
        sourceExport: source.root,
      }),
    ).rejects.toThrow("historical accepted archive cannot represent the changed candidate");
  });

  it("fails closed before lifecycle work when predecessor admission is unavailable", async () => {
    const root = await temporaryRoot();
    const candidate = await admittedArchive(root, "2026.36.2");
    const predecessor = await admittedArchive(root, "2026.36.1");
    const candidateSource = await sourceExport(root, "candidate-source", "candidate\n");
    const predecessorSource = await sourceExport(root, "predecessor-source", "predecessor\n");
    const candidateReceipt = await admissionReceipt(root, "candidate", candidateSource, candidate);
    const lifecycleRootsBefore = await lifecycleTemporaryRoots();

    await expect(
      validateLifecycleInputs({
        candidate: candidate.path,
        candidateSha256: candidate.sha256,
        candidateReceipt: candidateReceipt.path,
        candidateReceiptSha256: candidateReceipt.sha256,
        candidateSourceExport: candidateSource.root,
        predecessor: predecessor.path,
        predecessorSha256: predecessor.sha256,
        predecessorReceipt: join(root, "missing-predecessor-receipt.json"),
        predecessorReceiptSha256: "0".repeat(64),
        predecessorSourceExport: predecessorSource.root,
        catalog: join(root, "not-read.json"),
        catalogSha256: "1".repeat(64),
      }),
    ).rejects.toThrow("predecessor receipt is unavailable");
    expect(await lifecycleTemporaryRoots()).toEqual(lifecycleRootsBefore);
  });

  it("rejects a self-asserted or unaccepted predecessor receipt", async () => {
    const root = await temporaryRoot();
    const source = await sourceExport(root, "predecessor-source", "predecessor\n");
    const archive = await admittedArchive(root, "2026.36.1");
    const admission = await admissionReceipt(root, "predecessor", source, archive, {
      state: "draft",
    });

    await expect(
      validateReviewedPackageBinding({
        role: "predecessor",
        receiptPath: admission.path,
        receiptSha256: admission.sha256,
        archive,
        sourceExport: source.root,
      }),
    ).rejects.toThrow(
      "predecessor receipt does not bind accepted source/package/toolchain evidence",
    );
  });

  it("rejects an independently reviewed predecessor absent from the immutable catalog", async () => {
    const root = await temporaryRoot();
    const candidate = await admittedArchive(root, "2026.36.2");
    const predecessor = await admittedArchive(root, "2026.36.1");
    const candidateSource = await sourceExport(root, "candidate-source", "candidate\n");
    const predecessorSource = await sourceExport(root, "predecessor-source", "predecessor\n");
    const candidateReceipt = await admissionReceipt(root, "candidate", candidateSource, candidate);
    const predecessorReceipt = await admissionReceipt(
      root,
      "predecessor",
      predecessorSource,
      predecessor,
    );
    const candidateAdmission = await validateReviewedPackageBinding({
      role: "candidate",
      receiptPath: candidateReceipt.path,
      receiptSha256: candidateReceipt.sha256,
      archive: candidate,
      sourceExport: candidateSource.root,
    });
    const catalogPath = join(root, "catalog.json");
    await writeFile(catalogPath, `${JSON.stringify(catalog(candidateAdmission))}\n`);

    await expect(
      validateLifecycleInputs({
        candidate: candidate.path,
        candidateSha256: candidate.sha256,
        candidateReceipt: candidateReceipt.path,
        candidateReceiptSha256: candidateReceipt.sha256,
        candidateSourceExport: candidateSource.root,
        predecessor: predecessor.path,
        predecessorSha256: predecessor.sha256,
        predecessorReceipt: predecessorReceipt.path,
        predecessorReceiptSha256: predecessorReceipt.sha256,
        predecessorSourceExport: predecessorSource.root,
        catalog: catalogPath,
        catalogSha256: await sha256File(catalogPath),
      }),
    ).rejects.toThrow("catalog must contain exactly one package cohort");
  });

  it("validates all five exact companion identities and rejects placeholder components", async () => {
    const root = await temporaryRoot();
    const source = await sourceExport(root, "candidate-source", "candidate\n");
    const archive = await admittedArchive(root, "2026.36.2");
    const receiptFile = await admissionReceipt(root, "candidate", source, archive);
    const reviewedPackage = await validateReviewedPackageBinding({
      role: "candidate",
      receiptPath: receiptFile.path,
      receiptSha256: receiptFile.sha256,
      archive,
      sourceExport: source.root,
    });
    const catalogPath = join(root, "catalog.json");
    const catalogValue = catalog(reviewedPackage);
    await writeFile(catalogPath, `${JSON.stringify(catalogValue)}\n`);
    const catalogSha256 = await sha256File(catalogPath);

    await expect(
      readCatalogBinding({ catalogPath, catalogSha256, reviewedPackage }),
    ).resolves.toMatchObject({
      sourceCommit: reviewedPackage.receipt.source.commit,
      packageSha256: archive.sha256,
      packageMemberCount: archive.memberCount,
      admissionReceiptSha256: receiptFile.sha256,
    });
    const invalidCatalog = structuredClone(catalogValue) as {
      cohorts: Array<{ components: Record<string, unknown> }>;
    };
    const invalidCohort = invalidCatalog.cohorts[0];
    if (invalidCohort === undefined) throw new Error("test catalog cohort is missing");
    invalidCohort.components.protocol = {};
    await writeFile(catalogPath, `${JSON.stringify(invalidCatalog)}\n`);
    await expect(
      readCatalogBinding({
        catalogPath,
        catalogSha256: await sha256File(catalogPath),
        reviewedPackage,
      }),
    ).rejects.toThrow("catalog protocol fields mismatch");
  });

  it("binds the separate final Hub identity, package admission and companion catalog", () => {
    const packageBinding = {
      sha256: "4".repeat(64),
      packageVersion: "2026.36.2",
      memberCount: 86,
    };
    const catalogBinding = {
      sha256: "5".repeat(64),
      sourceCommit: "6".repeat(40),
      sourceSha256: "7".repeat(64),
      packageMemberCount: packageBinding.memberCount,
      admissionReceiptSha256: "b".repeat(64),
      admittedHubVersions: ["2026.36.2"],
    };
    const hub = {
      repository: "https://github.com/magrathean-uk/teslatlas-hub.git",
      product_version: "2026.36.2",
      source_commit: "8".repeat(40),
      source_sha256: "8".repeat(64),
      artifact_kind: "native",
      artifact_sha256: "9".repeat(64),
      profile,
    };
    const target = {
      schema_version: 1,
      state: "final-f1-artifact-ready",
      hub_id: "00000000-0000-4000-8000-000000000001",
      hub,
      sdk: {
        source_commit: catalogBinding.sourceCommit,
        source_sha256: catalogBinding.sourceSha256,
        package_sha256: packageBinding.sha256,
        package_version: packageBinding.packageVersion,
        package_member_count: packageBinding.memberCount,
        admission_receipt_sha256: catalogBinding.admissionReceiptSha256,
      },
      descriptor_sha256: "a".repeat(64),
      catalog_sha256: catalogBinding.sha256,
    };
    const descriptor = { hub_id: target.hub_id, acceptance_target: hub };

    expect(
      validateFinalTarget(target, descriptor, packageBinding, catalogBinding, {
        sha256: target.descriptor_sha256,
      }),
    ).toEqual({ hub, sdk: target.sdk, hubId: target.hub_id });
  });

  it("attempts image and context cleanup and aggregates both failures", async () => {
    const calls: string[] = [];
    await expect(
      cleanupDockerArtifacts({
        tag: "task-owned",
        context: "/private/tmp/task-owned",
        removeImage: () => {
          calls.push("image");
          throw new Error("image cleanup failed");
        },
        removeContext: async () => {
          calls.push("context");
          throw new Error("context cleanup failed");
        },
      }),
    ).rejects.toMatchObject({
      errors: [{ message: "image cleanup failed" }, { message: "context cleanup failed" }],
    });
    expect(calls).toEqual(["image", "context"]);
  });
});

function lifecycleTemporaryRoots() {
  return readdir(tmpdir()).then((names) =>
    names.filter((name) => name.startsWith("teslatlas-package-lifecycle-")),
  );
}

async function temporaryRoot() {
  const path = await realpath(await mkdtemp(join(tmpdir(), "teslatlas-package-gate-test-")));
  temporaryRoots.push(path);
  return path;
}

async function sourceExport(root: string, name: string, contents: string) {
  const path = join(root, name);
  await mkdir(path, { mode: 0o700 });
  await writeFile(join(path, "README.md"), contents, { mode: 0o600 });
  return sourceExportManifest(path);
}

async function admittedArchive(root: string, version: string) {
  const result = await packageArchive(root, version);
  return inspectPackageArchive(result.path, result.sha256);
}

async function packageArchive(
  root: string,
  version: string,
  additions: Array<{ name: string; type: string; mode: number }> = [],
) {
  const manifest = Buffer.from(
    `${JSON.stringify({
      name: "@teslatlas/sdk",
      version,
      private: true,
      packageManager: "npm@11.19.0",
    })}\n`,
  );
  const entries = [
    { name: "package/package.json", type: "0", mode: 0o644, contents: manifest },
    ...additions.map((entry) => ({ ...entry, contents: Buffer.from("test\n") })),
  ];
  const path = join(root, `teslatlas-sdk-${version}.tgz`);
  await writeFile(path, gzipSync(tar(entries)));
  return { path, sha256: await sha256File(path) };
}

function tar(entries: Array<{ name: string; type: string; mode: number; contents: Buffer }>) {
  const blocks: Buffer[] = [];
  for (const entry of entries) {
    const header = Buffer.alloc(512);
    header.write(entry.name, 0, 100, "utf8");
    tarOctal(header, entry.mode, 100, 8);
    tarOctal(header, 0, 108, 8);
    tarOctal(header, 0, 116, 8);
    const size = entry.type === "0" ? entry.contents.length : 0;
    tarOctal(header, size, 124, 12);
    tarOctal(header, 0, 136, 12);
    header.fill(0x20, 148, 156);
    header.write(entry.type, 156, 1, "ascii");
    header.write("ustar\0", 257, 6, "ascii");
    header.write("00", 263, 2, "ascii");
    let checksum = 0;
    for (const byte of header) checksum += byte;
    const checksumText = checksum.toString(8).padStart(6, "0");
    header.write(checksumText, 148, 6, "ascii");
    header[154] = 0;
    header[155] = 0x20;
    blocks.push(header);
    if (size > 0) {
      blocks.push(entry.contents, Buffer.alloc(Math.ceil(size / 512) * 512 - size));
    }
  }
  blocks.push(Buffer.alloc(1024));
  return Buffer.concat(blocks);
}

function tarOctal(buffer: Buffer, value: number, offset: number, length: number) {
  const text = value.toString(8).padStart(length - 1, "0");
  buffer.write(text, offset, length - 1, "ascii");
  buffer[offset + length - 1] = 0;
}

async function admissionReceipt(
  root: string,
  role: "candidate" | "predecessor",
  source: { root: string; manifestSha256: string; fileCount: number },
  archive: {
    sha256: string;
    memberCount: number;
    packageName: string;
    packageVersion: string;
  },
  overrides: { state?: string } = {},
) {
  const value = {
    schema_version: 1,
    kind: "teslatlas.sdk-package-admission",
    role,
    state: overrides.state ?? "independently-reviewed",
    product_version: archive.packageVersion,
    source: {
      repository: "https://github.com/magrathean-uk/teslatlas-sdk-typescript.git",
      commit: role === "candidate" ? "c".repeat(40) : "d".repeat(40),
      manifest_sha256: source.manifestSha256,
      file_count: source.fileCount,
    },
    package: {
      name: archive.packageName,
      version: archive.packageVersion,
      archive_sha256: archive.sha256,
      member_count: archive.memberCount,
    },
    toolchain: { node: "26.7.0", npm: "11.19.0" },
    review: {
      verdict: "ACCEPT",
      reviewer_model: "GPT-5.6 Sol",
      reasoning: "high",
      reviewed_at: "2026-09-19T16:00:00Z",
    },
  };
  const path = join(root, `${role}-admission.json`);
  await writeFile(path, `${JSON.stringify(value)}\n`);
  return { path, sha256: await sha256File(path), value };
}

function catalog(reviewedPackage: Awaited<ReturnType<typeof validateReviewedPackageBinding>>) {
  const version = reviewedPackage.receipt.product_version;
  return {
    schema_version: 1,
    cohorts: [
      {
        product_version: version,
        publication_status: "local-unpublished",
        admitted_hub_versions: [version],
        components: {
          protocol: component("protocol", version, "2"),
          "sdk-typescript": {
            ...component("sdk-typescript", version, "3"),
            commit: reviewedPackage.receipt.source.commit,
            source_sha256: reviewedPackage.source.catalogManifestSha256,
            artifacts: {
              package_filename: `teslatlas-sdk-${version}.tgz`,
              package_sha256: reviewedPackage.receipt.package.archive_sha256,
            },
          },
          "sdk-swift": component("sdk-swift", version, "4"),
          "home-assistant": {
            ...component("home-assistant", version, "5"),
            artifacts: {
              payload_manifest_sha256: "6".repeat(64),
              selection_receipt_sha256: "7".repeat(64),
            },
          },
          edge: component("edge", version, "8"),
        },
      },
    ],
  };
}

function component(name: string, version: string, marker: string) {
  const repositories: Record<string, string> = {
    protocol: "https://github.com/magrathean-uk/teslatlas-protocol.git",
    "sdk-typescript": "https://github.com/magrathean-uk/teslatlas-sdk-typescript.git",
    "sdk-swift": "https://github.com/magrathean-uk/teslatlas-sdk-swift.git",
    "home-assistant": "https://github.com/magrathean-uk/teslatlas-home-assistant.git",
    edge: "https://github.com/magrathean-uk/teslatlas-edge.git",
  };
  const repository = repositories[name];
  if (repository === undefined) throw new Error(`unknown test component ${name}`);
  return {
    repository,
    commit: marker.repeat(40),
    source_sha256: marker.repeat(64),
    product_version: version,
    profile: name === "edge" ? edgeProfile : profile,
  };
}
