import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import {
  discoverJsonFiles,
  filesForProfile,
  readProtocolLock,
  sha256DigestMap,
  sha256File,
} from "./protocol-files.mjs";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const sourceRoot = join(repositoryRoot, "protocol/source");
const lock = await readProtocolLock(join(repositoryRoot, "protocol/lock.json"));
const richerGeneratedPaths = [
  "src/generated/protocol.ts",
  "src/generated/validators.ts",
  "src/generated/protocol-cases.ts",
];
const currentHubGeneratedPaths = [
  "src/generated/hub-protocol.ts",
  "src/generated/hub-validators.js",
  "src/generated/hub-validators.d.ts",
];
const generatedPaths = [...new Set([...richerGeneratedPaths, ...currentHubGeneratedPaths])];

const richerProfile = lock.profiles?.richer;
const currentHubProfile = lock.profiles?.currentHub;
const richerFiles = filesForProfile(lock.files ?? {}, "richer");
const currentHubFiles = filesForProfile(lock.files ?? {}, "currentHub");
const sourceIsImmutable =
  lock.source?.kind === "git-commit" && /^[0-9a-f]{40}$/.test(lock.source?.commit ?? "");
const sourceIsBoundCandidate =
  lock.source?.kind === "local-content" &&
  /^[0-9a-f]{40}$/.test(lock.source?.baseCommit ?? "") &&
  lock.source?.contentSha256 === sha256DigestMap(lock.files ?? {});
if (
  lock.schemaVersion !== 2 ||
  (!sourceIsImmutable && !sourceIsBoundCandidate) ||
  typeof richerProfile?.id !== "string" ||
  typeof richerProfile?.revision !== "string" ||
  !Array.isArray(richerProfile?.supportedRevisions) ||
  typeof currentHubProfile?.id !== "string" ||
  typeof currentHubProfile?.revision !== "string"
) {
  throw new Error("Protocol lock metadata is invalid");
}
const compatibilityManifest = JSON.parse(
  await readFile(join(sourceRoot, "compatibility/manifest.json"), "utf8"),
);
if (
  compatibilityManifest.current_version !== richerProfile.revision ||
  JSON.stringify(compatibilityManifest.supported_profiles) !==
    JSON.stringify(richerProfile.supportedRevisions)
) {
  throw new Error("Richer protocol profile metadata does not match its vendored manifest");
}
if (lock.generator?.package !== "openapi-typescript" || lock.generator?.version !== "7.13.0") {
  throw new Error("Protocol lock generator does not match the pinned generator");
}
if (sha256DigestMap(richerFiles) !== richerProfile.inputSha256) {
  throw new Error("Richer protocol input digest does not match the pinned file map");
}
if (
  sha256DigestMap(
    Object.fromEntries(richerGeneratedPaths.map((path) => [path, lock.generated?.[path]])),
  ) !== richerProfile.generatedOutputSha256
) {
  throw new Error("Richer protocol generated-output digest does not match the pinned file map");
}
if (
  currentHubProfile.status === "candidate" &&
  currentHubProfile.inputSha256 !==
    (Object.keys(currentHubFiles).length === 0 ? null : sha256DigestMap(currentHubFiles))
) {
  throw new Error("Current-Hub candidate input digest does not match its file map");
}
if (
  Object.keys(currentHubFiles).length > 0 &&
  currentHubProfile.bundleSha256 !==
    (await sha256File(join(sourceRoot, "profiles/hub-http-v1/1.0.0/SHA256SUMS")))
) {
  throw new Error("Current-Hub profile bundle digest does not match SHA256SUMS bytes");
}
if (
  Object.keys(currentHubFiles).length > 0 &&
  sha256DigestMap(
    Object.fromEntries(currentHubGeneratedPaths.map((path) => [path, lock.generated?.[path]])),
  ) !== currentHubProfile.generatedOutputSha256
) {
  throw new Error("Current-Hub generated-output digest does not match the pinned file map");
}
const files = await discoverJsonFiles(sourceRoot);
if (JSON.stringify(files) !== JSON.stringify(Object.keys(lock.files).sort())) {
  throw new Error("Protocol lock file list does not match vendored protocol inputs");
}
for (const path of files) {
  if ((await sha256File(join(sourceRoot, path))) !== lock.files[path])
    throw new Error(`Protocol source hash mismatch: ${path}`);
}
for (const path of generatedPaths) {
  if ((await sha256File(join(repositoryRoot, path))) !== lock.generated?.[path]) {
    throw new Error(`Protocol generated hash mismatch: ${path}`);
  }
}

const temporaryOutput = await mkdtemp(join(tmpdir(), "teslatlas-sdk-protocol-"));
try {
  execFileSync(process.execPath, ["scripts/generate-protocol.mjs"], {
    cwd: repositoryRoot,
    env: { ...process.env, TESLATLAS_PROTOCOL_OUTPUT_DIR: temporaryOutput },
    stdio: "pipe",
  });
  for (const path of generatedPaths) {
    const output = join(temporaryOutput, path.replace("src/generated/", ""));
    if ((await sha256File(output)) !== lock.generated?.[path])
      throw new Error(`Protocol generated hash mismatch: ${path}`);
  }
} finally {
  await rm(temporaryOutput, { recursive: true, force: true });
}
console.log("Protocol lock verified");
