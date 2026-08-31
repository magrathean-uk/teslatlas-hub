import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import { currentProfile, discoverJsonFiles, protocolCommit, sha256File, supportedProfiles } from "./protocol-files.mjs";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const sourceRoot = join(repositoryRoot, "protocol/source");
const lock = JSON.parse(await readFile(join(repositoryRoot, "protocol/lock.json"), "utf8"));
const generatedPaths = ["src/generated/protocol.ts", "src/generated/validators.ts", "src/generated/protocol-cases.ts"];

if (lock.commit !== protocolCommit || lock.currentProfile !== currentProfile || JSON.stringify(lock.supportedProfiles) !== JSON.stringify(supportedProfiles)) {
  throw new Error("Protocol lock metadata does not match the pinned protocol inputs");
}
if (lock.generator?.package !== "openapi-typescript" || lock.generator?.version !== "7.13.0") {
  throw new Error("Protocol lock generator does not match the pinned generator");
}
const files = await discoverJsonFiles(sourceRoot);
if (JSON.stringify(files) !== JSON.stringify(Object.keys(lock.files).sort())) {
  throw new Error("Protocol lock file list does not match vendored protocol inputs");
}
for (const path of files) {
  if ((await sha256File(join(sourceRoot, path))) !== lock.files[path]) throw new Error(`Protocol source hash mismatch: ${path}`);
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
    if ((await sha256File(output)) !== lock.generated?.[path]) throw new Error(`Protocol generated hash mismatch: ${path}`);
  }
} finally {
  await rm(temporaryOutput, { recursive: true, force: true });
}
console.log("Protocol lock verified");
