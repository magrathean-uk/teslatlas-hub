import { execFileSync } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  inspectPackageArchive,
  parseStrictJson,
  readCatalogBinding,
  validateReviewedPackageBinding,
} from "./package-provenance.mjs";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const imageLockPath = join(repositoryRoot, "tools/node-image-lock.json");
// Updating the registry lock alone must never authorize an unreviewed image.
const trustedNodeImage = Object.freeze({
  tag: "26.10.0-bookworm-slim",
  index: "sha256:662933cf47f013bc8e4beb31a6116448427a82057ba7c42c97e4c5ba766504c2",
  arm64: "sha256:131c9695bc02b79f4dbb1ca360f5822d5f67f248a621498c22de5883f94122d0",
});

export function validateOfficialNodeImageLock(imageLock, dockerfile) {
  const baseImages = dockerfile.split(/\r?\n/u).filter((line) => /^FROM\s/iu.test(line));
  if (
    imageLock?.schema_version !== 1 ||
    imageLock.registry !== "registry-1.docker.io" ||
    imageLock.repository !== "library/node" ||
    imageLock.official_image !== true ||
    imageLock.tag_observed !== trustedNodeImage.tag ||
    imageLock.index_media_type !== "application/vnd.oci.image.index.v1+json" ||
    imageLock.index_digest !== trustedNodeImage.index ||
    imageLock.linux_arm64_v8_child_digest !== trustedNodeImage.arm64 ||
    baseImages[0] !==
      `FROM --platform=linux/arm64 node:${trustedNodeImage.tag}@${trustedNodeImage.index} AS package-check` ||
    baseImages.slice(1).some((line) => !/^FROM package-check AS (?:test|consumer)$/u.test(line))
  ) {
    throw new Error("official Node image provenance lock is invalid");
  }
  return imageLock;
}

export async function validateDockerGateInputs({
  tarball,
  sha256,
  candidateReceipt,
  candidateReceiptSha256,
  sourceExport,
  catalog,
  catalogSha256,
}) {
  const archive = await inspectPackageArchive(tarball, sha256, "Docker SDK archive");
  const candidateAdmission = await validateReviewedPackageBinding({
    role: "candidate",
    receiptPath: candidateReceipt,
    receiptSha256: candidateReceiptSha256,
    archive,
    sourceExport,
  });
  const catalogBinding = await readCatalogBinding({
    catalogPath: catalog,
    catalogSha256,
    reviewedPackage: candidateAdmission,
  });
  const imageLock = parseStrictJson(await readFile(imageLockPath, "utf8"), "Node image lock");
  validateOfficialNodeImageLock(
    imageLock,
    await readFile(join(repositoryRoot, "Dockerfile"), "utf8"),
  );
  return { archive, candidateAdmission, catalogBinding, imageLock };
}

export async function stageDockerPackageContext({ context, archivePath }) {
  await cp(join(repositoryRoot, "Dockerfile"), join(context, "Dockerfile"));
  await cp(archivePath, join(context, "teslatlas-sdk.tgz"));
  await cp(join(repositoryRoot, "docker/package-smoke.mjs"), join(context, "package-smoke.mjs"));
  const consumer = join(context, "consumer");
  await mkdir(consumer, { mode: 0o700 });
  await writeFile(
    join(consumer, "package.json"),
    `${JSON.stringify({ name: "teslatlas-docker-package-consumer", private: true, type: "module" })}\n`,
    { mode: 0o600, flag: "wx" },
  );
  for (const name of ["node.mjs", "index.html", "app.js", "serve.mjs"]) {
    await cp(join(repositoryRoot, "examples/hub", name), join(consumer, name));
  }
}

export async function runDockerPackageGate(options) {
  const admitted = await validateDockerGateInputs(options);
  const { sha256 } = options;
  const serverPlatform = run("docker", ["info", "--format", "{{.OSType}}/{{.Architecture}}"])
    .trim()
    .replace("linux/aarch64", "linux/arm64");
  if (serverPlatform !== "linux/arm64") {
    throw new Error("Docker gate requires a native linux/arm64 engine; emulation is forbidden");
  }
  const tag = `teslatlas-sdk-package-gate:${admitted.archive.packageVersion}-${sha256.slice(0, 12)}`;
  if (imageExists(tag)) throw new Error("task-owned Docker image tag already exists");
  const context = await mkdtemp(join(tmpdir(), "teslatlas-docker-package-"));
  let result;
  let primaryError;
  try {
    await stageDockerPackageContext({ context, archivePath: admitted.archive.path });
    run("docker", [
      "build",
      "--platform",
      "linux/arm64",
      "--target",
      "consumer",
      "--build-arg",
      `SDK_TARBALL_SHA256=${sha256}`,
      "--tag",
      tag,
      context,
    ]);
    const imageId = run("docker", ["image", "inspect", "--format", "{{.Id}}", tag]).trim();
    if (!/^sha256:[0-9a-f]{64}$/u.test(imageId)) throw new Error("Docker image ID is invalid");
    result = {
      result: "DOCKER_PACKAGE_BUILD_PASS",
      platform: serverPlatform,
      package: admitted.archive,
      candidateAdmission: admitted.candidateAdmission,
      catalog: admitted.catalogBinding,
      baseImage: admitted.imageLock,
      imageId,
      cleanup: "temporary image and context removed",
    };
  } catch (error) {
    primaryError = error;
  }
  let cleanupError;
  try {
    await cleanupDockerArtifacts({ tag, context });
  } catch (error) {
    cleanupError = error;
  }
  if (primaryError !== undefined && cleanupError !== undefined) {
    throw new AggregateError(
      [primaryError, ...(cleanupError.errors ?? [cleanupError])],
      "Docker package gate and cleanup failed",
    );
  }
  if (primaryError !== undefined) throw primaryError;
  if (cleanupError !== undefined) throw cleanupError;
  if (result === undefined) throw new Error("Docker package gate produced no result");
  return result;
}

export async function cleanupDockerArtifacts({
  tag,
  context,
  removeImage = removeImageIfPresent,
  removeContext = (path) => rm(path, { recursive: true, force: true }),
}) {
  const errors = [];
  try {
    removeImage(tag);
  } catch (error) {
    errors.push(error);
  }
  try {
    await removeContext(context);
  } catch (error) {
    errors.push(error);
  }
  if (errors.length > 0) throw new AggregateError(errors, "Docker package gate cleanup failed");
}

function removeImageIfPresent(tag) {
  try {
    run("docker", ["image", "rm", "--force", tag]);
  } catch (error) {
    const detail = `${error?.message ?? ""}\n${error?.stderr ?? ""}\n${error?.stdout ?? ""}`;
    if (!detail.includes("No such image")) throw error;
  }
}

function imageExists(tag) {
  try {
    run("docker", ["image", "inspect", tag]);
    return true;
  } catch {
    return false;
  }
}

function run(executable, arguments_) {
  return execFileSync(executable, arguments_, {
    cwd: repositoryRoot,
    encoding: "utf8",
    env: { ...process.env, DOCKER_BUILDKIT: "1" },
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function parseArguments(values) {
  const names = new Map([
    ["--tarball", "tarball"],
    ["--sha256", "sha256"],
    ["--candidate-receipt", "candidateReceipt"],
    ["--candidate-receipt-sha256", "candidateReceiptSha256"],
    ["--source-export", "sourceExport"],
    ["--catalog", "catalog"],
    ["--catalog-sha256", "catalogSha256"],
  ]);
  const options = {};
  for (let index = 0; index < values.length; index += 2) {
    const name = names.get(values[index]);
    const value = values[index + 1];
    if (name === undefined || value === undefined || value.startsWith("--") || options[name]) {
      throw new Error("invalid or duplicate Docker package gate option");
    }
    options[name] = value;
  }
  for (const name of names.values()) {
    if (options[name] === undefined)
      throw new Error(`Docker package gate option ${name} is required`);
  }
  return options;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  runDockerPackageGate(parseArguments(process.argv.slice(2)))
    .then((receipt) => process.stdout.write(`${JSON.stringify(receipt)}\n`))
    .catch((error) => {
      process.stderr.write(`Docker package gate: ${error.message}\n`);
      process.exitCode = 1;
    });
}
