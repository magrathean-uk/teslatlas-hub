import { execFileSync } from "node:child_process";
import { chmod, mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { verifyPackedSdk } from "./hub-acceptance-evidence.mjs";
import {
  compareProductVersions,
  inspectPackageArchive,
  readCatalogBinding,
  validateReviewedPackageBinding,
} from "./package-provenance.mjs";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const npmExecutable = process.platform === "win32" ? "npm.cmd" : "npm";

export async function validateLifecycleInputs(options) {
  const candidate = await inspectPackageArchive(
    options.candidate,
    options.candidateSha256,
    "candidate archive",
  );
  const predecessor = await inspectPackageArchive(
    options.predecessor,
    options.predecessorSha256,
    "predecessor archive",
  );
  if (compareProductVersions(predecessor.packageVersion, candidate.packageVersion) !== -1) {
    throw new Error("predecessor package version must be older than the candidate");
  }
  const candidateAdmission = await validateReviewedPackageBinding({
    role: "candidate",
    receiptPath: options.candidateReceipt,
    receiptSha256: options.candidateReceiptSha256,
    archive: candidate,
    sourceExport: options.candidateSourceExport,
  });
  const predecessorAdmission = await validateReviewedPackageBinding({
    role: "predecessor",
    receiptPath: options.predecessorReceipt,
    receiptSha256: options.predecessorReceiptSha256,
    archive: predecessor,
    sourceExport: options.predecessorSourceExport,
  });
  const candidateCatalog = await readCatalogBinding({
    catalogPath: options.catalog,
    catalogSha256: options.catalogSha256,
    reviewedPackage: candidateAdmission,
  });
  const predecessorCatalog = await readCatalogBinding({
    catalogPath: options.catalog,
    catalogSha256: options.catalogSha256,
    reviewedPackage: predecessorAdmission,
  });
  return {
    candidate,
    predecessor,
    candidateAdmission,
    predecessorAdmission,
    candidateCatalog,
    predecessorCatalog,
  };
}

export async function runLifecycle(options) {
  // All immutable inputs, including the predecessor, are admitted before a
  // temporary consumer is created or npm can change state.
  const admitted = await validateLifecycleInputs(options);
  assertExactToolchain();
  const temporaryRoot = await mkdtemp(join(tmpdir(), "teslatlas-package-lifecycle-"));
  const consumer = join(temporaryRoot, "consumer");
  const installed = join(consumer, "node_modules/@teslatlas/sdk");
  const phases = [];
  let receipt;
  try {
    await mkdir(consumer, { mode: 0o700 });
    await writeFile(
      join(consumer, "package.json"),
      '{"name":"teslatlas-external-lifecycle-consumer","private":true,"type":"module"}\n',
      { mode: 0o600, flag: "wx" },
    );

    install(consumer, admitted.candidate.path);
    phases.push(await status("fresh-install", consumer, installed, admitted.candidate));
    uninstall(consumer);
    await assertRemoved(installed);
    assertImportUnavailable(consumer);
    phases.push({ phase: "fresh-removal", packageAbsent: true });

    install(consumer, admitted.predecessor.path);
    phases.push(await status("predecessor-install", consumer, installed, admitted.predecessor));
    install(consumer, admitted.candidate.path);
    phases.push(await status("update", consumer, installed, admitted.candidate));
    install(consumer, admitted.predecessor.path);
    phases.push(await status("rollback", consumer, installed, admitted.predecessor));
    uninstall(consumer);
    await assertRemoved(installed);
    assertImportUnavailable(consumer);
    phases.push({ phase: "final-removal", packageAbsent: true });

    receipt = {
      schemaVersion: 1,
      result: "PACKAGE_LIFECYCLE_PASS",
      candidateAdmission: admitted.candidateAdmission,
      predecessorAdmission: admitted.predecessorAdmission,
      candidateCatalog: admitted.candidateCatalog,
      predecessorCatalog: admitted.predecessorCatalog,
      candidate: admitted.candidate,
      predecessor: admitted.predecessor,
      toolchain: { node: process.version, npm: npmVersion() },
      phases,
      cleanup: "temporary consumer removed by the gate",
    };
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
  if (receipt === undefined) throw new Error("package lifecycle produced no receipt");
  if (options.receipt !== undefined) await writeReceipt(options.receipt, receipt);
  return receipt;
}

async function status(phase, consumer, installed, archive) {
  const verified = await verifyPackedSdk({
    packageRoot: installed,
    tarballPath: archive.path,
    expectedTarballSha256: archive.sha256,
    entryKind: "node",
  });
  assertImports(consumer);
  return { phase, imports: "passed", ...verified.witness };
}

const importCheck = `
  const root = await import("@teslatlas/sdk");
  const node = await import("@teslatlas/sdk/node");
  const browser = await import("@teslatlas/sdk/browser");
  if (typeof root.TeslatlasError !== "function" || typeof node.createClient !== "function" || typeof browser.createClient !== "function") throw new Error("public imports failed");
`;

function assertImports(consumer) {
  run(process.execPath, ["--input-type=module", "--eval", importCheck], consumer);
}

function assertImportUnavailable(consumer) {
  try {
    assertImports(consumer);
  } catch {
    return;
  }
  throw new Error("SDK imports remain available after removal");
}

function install(consumer, archive) {
  run(
    npmExecutable,
    [
      "install",
      "--no-save",
      "--package-lock=false",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      archive,
    ],
    consumer,
  );
}

function uninstall(consumer) {
  run(
    npmExecutable,
    [
      "uninstall",
      "--no-save",
      "--package-lock=false",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      "@teslatlas/sdk",
    ],
    consumer,
  );
}

async function assertRemoved(installed) {
  try {
    await realpath(installed);
  } catch (error) {
    if (error?.code === "ENOENT") return;
    throw error;
  }
  throw new Error("SDK package remains after removal");
}

function npmVersion() {
  return run(npmExecutable, ["--version"], repositoryRoot).trim();
}

function assertExactToolchain() {
  if (process.version !== "v26.7.0" || npmVersion() !== "11.19.0") {
    throw new Error("lifecycle gate requires exact Node v26.7.0 and npm 11.19.0");
  }
}

function run(executable, arguments_, cwd) {
  return execFileSync(executable, arguments_, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

async function writeReceipt(receiptPath, receipt) {
  if (!isAbsolute(receiptPath) || resolve(receiptPath) !== receiptPath) {
    throw new Error("receipt must be an absolute path");
  }
  const parent = await realpath(dirname(receiptPath));
  if (join(parent, basename(receiptPath)) !== receiptPath) {
    throw new Error("receipt parent must be canonical");
  }
  await writeFile(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`, {
    mode: 0o600,
    flag: "wx",
  });
  await chmod(receiptPath, 0o600);
}

export function parseLifecycleArguments(values) {
  const options = {};
  const names = new Map([
    ["--candidate", "candidate"],
    ["--candidate-sha256", "candidateSha256"],
    ["--candidate-receipt", "candidateReceipt"],
    ["--candidate-receipt-sha256", "candidateReceiptSha256"],
    ["--candidate-source-export", "candidateSourceExport"],
    ["--predecessor", "predecessor"],
    ["--predecessor-sha256", "predecessorSha256"],
    ["--predecessor-receipt", "predecessorReceipt"],
    ["--predecessor-receipt-sha256", "predecessorReceiptSha256"],
    ["--predecessor-source-export", "predecessorSourceExport"],
    ["--catalog", "catalog"],
    ["--catalog-sha256", "catalogSha256"],
    ["--receipt", "receipt"],
  ]);
  for (let index = 0; index < values.length; index += 2) {
    const option = values[index];
    const name = names.get(option);
    const value = values[index + 1];
    if (name === undefined || value === undefined || value.startsWith("--") || options[name]) {
      throw new Error("invalid or duplicate package lifecycle option");
    }
    options[name] = value;
  }
  for (const name of [
    "candidate",
    "candidateSha256",
    "candidateReceipt",
    "candidateReceiptSha256",
    "candidateSourceExport",
    "predecessor",
    "predecessorSha256",
    "predecessorReceipt",
    "predecessorReceiptSha256",
    "predecessorSourceExport",
    "catalog",
    "catalogSha256",
  ]) {
    if (options[name] === undefined) throw new Error(`--${camelToHyphen(name)} is required`);
  }
  return options;
}

function camelToHyphen(value) {
  return value.replaceAll(/[A-Z]/gu, (match) => `-${match.toLowerCase()}`);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  runLifecycle(parseLifecycleArguments(process.argv.slice(2)))
    .then((receipt) => process.stdout.write(`${JSON.stringify(receipt)}\n`))
    .catch((error) => {
      process.stderr.write(`Package lifecycle gate: ${error.message}\n`);
      process.exitCode = 1;
    });
}
