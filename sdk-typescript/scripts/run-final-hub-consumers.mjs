import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { isDeepStrictEqual } from "node:util";
import {
  canonicalRegularFile,
  inspectPackageArchive,
  parseStrictJson,
  readCatalogBinding,
  requireCommit,
  requireSha256,
  validateReviewedPackageBinding,
} from "./package-provenance.mjs";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const npmExecutable = process.platform === "win32" ? "npm.cmd" : "npm";
const profile = {
  id: "hub-http-v1",
  revision: "1.0.0",
  sha256: "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926",
};

export function validateFinalTarget(
  target,
  descriptor,
  packageBinding,
  catalogBinding,
  descriptorBinding,
) {
  const hub = target?.hub;
  const sdk = target?.sdk;
  assertExactKeys(
    target,
    ["schema_version", "state", "hub_id", "hub", "sdk", "descriptor_sha256", "catalog_sha256"],
    "final Hub target",
  );
  assertExactKeys(
    hub,
    [
      "repository",
      "product_version",
      "source_commit",
      "source_sha256",
      "artifact_kind",
      "artifact_sha256",
      "profile",
    ],
    "final Hub identity",
  );
  assertExactKeys(
    sdk,
    [
      "source_commit",
      "source_sha256",
      "package_sha256",
      "package_version",
      "package_member_count",
      "admission_receipt_sha256",
    ],
    "final SDK identity",
  );
  assertExactKeys(hub?.profile, ["id", "revision", "sha256"], "final Hub profile");
  if (
    target?.schema_version !== 1 ||
    target?.state !== "final-f1-artifact-ready" ||
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/u.test(target?.hub_id ?? "") ||
    !["native", "container"].includes(hub?.artifact_kind) ||
    !/^[0-9]{4}\.[0-9]{1,2}\.[0-9]+$/u.test(hub?.product_version ?? "") ||
    hub?.repository !== "https://github.com/magrathean-uk/teslatlas-hub.git" ||
    hub?.source_commit !== requireCommit(hub?.source_commit, "Hub source commit") ||
    hub?.source_sha256 !== requireSha256(hub?.source_sha256, "Hub source SHA-256") ||
    hub?.artifact_sha256 !== requireSha256(hub?.artifact_sha256, "Hub artifact SHA-256") ||
    !isDeepStrictEqual(hub?.profile, profile) ||
    descriptor?.hub_id !== target?.hub_id ||
    descriptor?.acceptance_target === undefined ||
    !isDeepStrictEqual(descriptor.acceptance_target, hub) ||
    sdk?.source_commit !== catalogBinding.sourceCommit ||
    sdk?.source_sha256 !== catalogBinding.sourceSha256 ||
    sdk?.package_sha256 !== packageBinding.sha256 ||
    sdk?.package_version !== packageBinding.packageVersion ||
    sdk?.package_member_count !== catalogBinding.packageMemberCount ||
    sdk?.admission_receipt_sha256 !== catalogBinding.admissionReceiptSha256 ||
    target?.descriptor_sha256 !== descriptorBinding?.sha256 ||
    target?.catalog_sha256 !== catalogBinding.sha256 ||
    !catalogBinding.admittedHubVersions.includes(hub.product_version)
  ) {
    throw new Error("final Hub/package/catalog target metadata does not match exact inputs");
  }
  return { hub, sdk, hubId: target.hub_id };
}

function assertExactKeys(value, expected, label) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (JSON.stringify(actual) !== JSON.stringify(wanted)) {
    throw new Error(`${label} fields mismatch`);
  }
}

export async function runFinalHubConsumers(environment = process.env) {
  const targetPath = await canonicalRegularFile(
    required(environment, "TESLATLAS_FINAL_HUB_TARGET"),
    "final Hub target",
  );
  const descriptorPath = await canonicalRegularFile(
    required(environment, "TESLATLAS_HUB_HTTP_CONFIG"),
    "Hub descriptor",
  );
  const targetBytes = await readFile(targetPath);
  const targetSha256 = createHash("sha256").update(targetBytes).digest("hex");
  if (targetSha256 !== required(environment, "TESLATLAS_FINAL_HUB_TARGET_SHA256")) {
    throw new Error("final Hub target SHA-256 mismatch");
  }
  const descriptorBytes = await readFile(descriptorPath);
  const descriptorBinding = {
    sha256: createHash("sha256").update(descriptorBytes).digest("hex"),
  };
  const target = parseStrictJson(
    new TextDecoder("utf-8", { fatal: true }).decode(targetBytes),
    "final Hub target",
  );
  const descriptor = parseStrictJson(
    new TextDecoder("utf-8", { fatal: true }).decode(descriptorBytes),
    "Hub descriptor",
  );
  const packageBinding = await inspectPackageArchive(
    required(environment, "TESLATLAS_HUB_SDK_TARBALL"),
    required(environment, "TESLATLAS_HUB_SDK_TARBALL_SHA256"),
    "final SDK archive",
  );
  const candidateAdmission = await validateReviewedPackageBinding({
    role: "candidate",
    receiptPath: required(environment, "TESLATLAS_HUB_SDK_CANDIDATE_RECEIPT"),
    receiptSha256: required(environment, "TESLATLAS_HUB_SDK_CANDIDATE_RECEIPT_SHA256"),
    archive: packageBinding,
    sourceExport: required(environment, "TESLATLAS_HUB_SDK_SOURCE_EXPORT"),
  });
  const catalogBinding = await readCatalogBinding({
    catalogPath: required(environment, "TESLATLAS_HUB_CATALOG"),
    catalogSha256: required(environment, "TESLATLAS_HUB_CATALOG_SHA256"),
    reviewedPackage: candidateAdmission,
  });
  const admitted = validateFinalTarget(
    target,
    descriptor,
    packageBinding,
    catalogBinding,
    descriptorBinding,
  );
  assertExactToolchain();

  const temporaryRoot = await mkdtemp(join(tmpdir(), "teslatlas-final-consumers-"));
  const consumer = join(temporaryRoot, "external-node-consumer");
  const nodeReceiptPath = join(temporaryRoot, "node-receipt.json");
  const browserReceiptPath = join(temporaryRoot, "browser-receipt.json");
  const credentialPath = join(temporaryRoot, "credential.json");
  const stagedDescriptorPath = join(temporaryRoot, "hub-descriptor.json");
  let receipt;
  try {
    await mkdir(consumer, { mode: 0o700 });
    await writeFile(
      join(consumer, "package.json"),
      '{"name":"teslatlas-final-external-consumer","private":true,"type":"module"}\n',
      { mode: 0o600, flag: "wx" },
    );
    await writeFile(stagedDescriptorPath, descriptorBytes, { mode: 0o600, flag: "wx" });
    run(
      npmExecutable,
      [
        "install",
        "--no-save",
        "--package-lock=false",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        packageBinding.path,
      ],
      consumer,
    );
    const packageRoot = await realpath(join(consumer, "node_modules/@teslatlas/sdk"));
    const common = {
      ...environment,
      TESLATLAS_HUB_SDK_PACKAGE_ROOT: packageRoot,
      TESLATLAS_HUB_SDK_TARBALL: packageBinding.path,
      TESLATLAS_HUB_SDK_TARBALL_SHA256: packageBinding.sha256,
      TESLATLAS_HUB_HTTP_CONFIG: stagedDescriptorPath,
    };
    run(process.execPath, [join(repositoryRoot, "scripts/test-hub-node.mjs")], repositoryRoot, {
      ...common,
      TESLATLAS_HUB_RECEIPT: nodeReceiptPath,
      TESLATLAS_HUB_CREDENTIAL_OUT: credentialPath,
    });
    run(process.execPath, [join(repositoryRoot, "scripts/test-hub-browser.mjs")], repositoryRoot, {
      ...common,
      TESLATLAS_HUB_RECEIPT: browserReceiptPath,
      TESLATLAS_HUB_CREDENTIAL_FILE: credentialPath,
    });
    const nodeReceipt = parseStrictJson(await readFile(nodeReceiptPath, "utf8"), "Node receipt");
    const browserReceipt = parseStrictJson(
      await readFile(browserReceiptPath, "utf8"),
      "browser receipt",
    );
    if (
      nodeReceipt.hubId !== admitted.hubId ||
      browserReceipt.hubId !== admitted.hubId ||
      nodeReceipt.packedSdk?.tarballSha256 !== packageBinding.sha256 ||
      browserReceipt.packedSdk?.tarballSha256 !== packageBinding.sha256
    ) {
      throw new Error("consumer receipts do not match the admitted final target");
    }
    receipt = {
      schemaVersion: 1,
      result: "FINAL_HUB_NODE_BROWSER_CONSUMERS_PASS",
      target,
      targetSha256,
      descriptorSha256: descriptorBinding.sha256,
      catalog: catalogBinding,
      candidateAdmission,
      package: packageBinding,
      node: nodeReceipt,
      browser: browserReceipt,
      cleanup: "external consumer, credential and intermediate receipts removed",
    };
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
  if (receipt === undefined) throw new Error("final consumer gate produced no receipt");
  if (environment.TESLATLAS_FINAL_HUB_RECEIPT !== undefined) {
    await writeReceipt(environment.TESLATLAS_FINAL_HUB_RECEIPT, receipt);
  }
  return receipt;
}

function required(environment, name) {
  const value = environment[name];
  if (typeof value !== "string" || value.length === 0) throw new Error(`${name} is required`);
  return value;
}

function assertExactToolchain() {
  const npmVersion = run(npmExecutable, ["--version"], repositoryRoot).trim();
  if (process.version !== "v26.7.0" || npmVersion !== "11.19.0") {
    throw new Error("final consumer gate requires exact Node v26.7.0 and npm 11.19.0");
  }
}

function run(executable, arguments_, cwd, environment = process.env) {
  return execFileSync(executable, arguments_, {
    cwd,
    env: environment,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

async function writeReceipt(receiptPath, receipt) {
  if (!isAbsolute(receiptPath) || resolve(receiptPath) !== receiptPath) {
    throw new Error("final receipt must be an absolute path");
  }
  const parent = await realpath(dirname(receiptPath));
  if (join(parent, basename(receiptPath)) !== receiptPath) {
    throw new Error("final receipt parent must be canonical");
  }
  await writeFile(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`, {
    mode: 0o600,
    flag: "wx",
  });
  await chmod(receiptPath, 0o600);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  runFinalHubConsumers()
    .then((receipt) => process.stdout.write(`${JSON.stringify(receipt)}\n`))
    .catch((error) => {
      process.stderr.write(`Final Hub consumer gate: ${error.message}\n`);
      process.exitCode = 1;
    });
}
