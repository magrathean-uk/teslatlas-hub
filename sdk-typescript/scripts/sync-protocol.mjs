import { execFileSync } from "node:child_process";
import { cp, lstat, mkdir, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import {
  assertContained,
  candidateSourceIdentity,
  discoverJsonFiles,
  filesForProfile,
  matchesSourcePath,
  readProtocolLock,
  sha256DigestMap,
  sha256File,
  stableJson,
} from "./protocol-files.mjs";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const sourceRoot = join(repositoryRoot, "protocol/source");
const lockPath = join(repositoryRoot, "protocol/lock.json");
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
const candidateMode = process.argv[2] === "--candidate";
const checkout = process.argv[candidateMode ? 3 : 2];
const existingLock = await readProtocolLock(lockPath);

if (
  typeof checkout !== "string" ||
  !checkout.startsWith("/") ||
  process.argv.length !== (candidateMode ? 4 : 3)
) {
  throw new Error(
    "Usage: node scripts/sync-protocol.mjs [--candidate] /absolute/path/to/teslatlas-protocol",
  );
}
const authorityRoot = resolve(checkout);
const authorityStat = await lstat(authorityRoot);
if (!authorityStat.isDirectory() || authorityStat.isSymbolicLink()) {
  throw new Error("Protocol checkout must be a real directory");
}
const authorityCommit = execFileSync("git", ["-C", authorityRoot, "rev-parse", "HEAD"], {
  encoding: "utf8",
}).trim();
if (
  !candidateMode &&
  (existingLock.source?.kind !== "git-commit" || authorityCommit !== existingLock.source?.commit)
) {
  throw new Error(
    `Protocol checkout must be at ${existingLock.source?.commit ?? "the manifest-pinned commit"}, got ${authorityCommit}`,
  );
}

let files;
let committedInputs;
if (candidateMode) {
  files = await discoverJsonFiles(authorityRoot);
  committedInputs = null;
} else {
  committedInputs = committedProtocolInputs(authorityRoot, authorityCommit);
  files = [...committedInputs.keys()].sort();
}
await rm(sourceRoot, { recursive: true, force: true });
for (const path of files) {
  const destination = assertContained(sourceRoot, join(sourceRoot, path));
  await mkdir(dirname(destination), { recursive: true });
  if (candidateMode) {
    const source = assertContained(authorityRoot, join(authorityRoot, path));
    const stat = await lstat(source);
    if (!stat.isFile() || stat.isSymbolicLink())
      throw new Error(`Protocol input must be a regular file: ${source}`);
    await cp(source, destination, { dereference: false });
  } else {
    await writeFile(destination, committedInputs.get(path));
  }
}

const lock = {
  files: Object.fromEntries(
    await Promise.all(files.map(async (path) => [path, await sha256File(join(sourceRoot, path))])),
  ),
  generated: {},
  generator: existingLock.generator,
  profiles: structuredClone(existingLock.profiles),
  schemaVersion: existingLock.schemaVersion,
  source: existingLock.source,
};
lock.profiles.richer.inputSha256 = sha256DigestMap(filesForProfile(lock.files, "richer"));
const currentHubFiles = filesForProfile(lock.files, "currentHub");
lock.profiles.currentHub.inputSha256 =
  Object.keys(currentHubFiles).length === 0 ? null : sha256DigestMap(currentHubFiles);
lock.profiles.currentHub.generatedOutputSha256 = null;
lock.profiles.currentHub.bundleSha256 =
  Object.keys(currentHubFiles).length === 0
    ? null
    : await sha256File(join(sourceRoot, "profiles/hub-http-v1/1.0.0/SHA256SUMS"));
lock.profiles.currentHub.status = "candidate";
if (candidateMode) {
  lock.source = candidateSourceIdentity(
    existingLock.source.repository,
    authorityCommit,
    lock.files,
  );
}
await mkdir(dirname(lockPath), { recursive: true });
await writeFile(lockPath, stableJson(lock));

function committedProtocolInputs(checkout, commit) {
  const tree = execFileSync("git", ["-C", checkout, "ls-tree", "-r", "-z", "--full-tree", commit], {
    encoding: "buffer",
    maxBuffer: 16 * 1024 * 1024,
  });
  const inputs = new Map();
  for (const entry of tree.toString("utf8").split("\0")) {
    if (entry.length === 0) continue;
    const separator = entry.indexOf("\t");
    if (separator < 0) throw new Error("Git tree entry is malformed");
    const [mode, type] = entry.slice(0, separator).split(" ");
    const path = entry.slice(separator + 1);
    if (!matchesSourcePath(path)) continue;
    if (type !== "blob" || (mode !== "100644" && mode !== "100755")) {
      throw new Error(`Committed protocol input must be a regular file: ${path}`);
    }
    inputs.set(
      path,
      execFileSync("git", ["-C", checkout, "show", `${commit}:${path}`], {
        encoding: "buffer",
        maxBuffer: 32 * 1024 * 1024,
      }),
    );
  }
  return inputs;
}
execFileSync(process.execPath, ["scripts/generate-protocol.mjs"], {
  cwd: repositoryRoot,
  stdio: "inherit",
});
lock.generated = Object.fromEntries(
  await Promise.all(
    generatedPaths.map(async (path) => [path, await sha256File(join(repositoryRoot, path))]),
  ),
);
lock.profiles.richer.generatedOutputSha256 = sha256DigestMap(
  Object.fromEntries(richerGeneratedPaths.map((path) => [path, lock.generated[path]])),
);
lock.profiles.currentHub.generatedOutputSha256 = sha256DigestMap(
  Object.fromEntries(currentHubGeneratedPaths.map((path) => [path, lock.generated[path]])),
);
await writeFile(lockPath, stableJson(lock));
