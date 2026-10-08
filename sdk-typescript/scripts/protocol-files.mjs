import { createHash } from "node:crypto";
import { lstat, readdir, readFile } from "node:fs/promises";
import { relative, resolve, sep } from "node:path";

export const sourceGlobs = Object.freeze([
  "openapi/teslatlas-v1.openapi.json",
  "events/teslatlas-v1.sse.json",
  "schemas/*.schema.json",
  "examples/**/*.json",
  "fixtures/**/*.json",
  "compatibility/**/*.json",
  "conformance/cases/*.json",
  "profiles/hub-http-v1/**/*.{json,SHA256SUMS}",
]);

export function assertContained(root, candidate) {
  const rootPath = resolve(root);
  const candidatePath = resolve(candidate);
  const pathFromRoot = relative(rootPath, candidatePath);
  if (pathFromRoot === ".." || pathFromRoot.startsWith(`..${sep}`)) {
    throw new Error(`Path escapes protocol root: ${candidate}`);
  }
  return candidatePath;
}

export function matchesSourcePath(path) {
  return (
    path === "openapi/teslatlas-v1.openapi.json" ||
    path === "events/teslatlas-v1.sse.json" ||
    /^schemas\/[^/]+\.schema\.json$/.test(path) ||
    /^(examples|fixtures|compatibility)\/.+\.json$/.test(path) ||
    /^conformance\/cases\/[^/]+\.json$/.test(path) ||
    /^profiles\/hub-http-v1\/[^/]+\/(?:[A-Za-z0-9][A-Za-z0-9._-]*\/)*(?:[A-Za-z0-9][A-Za-z0-9._-]*\.json|SHA256SUMS)$/.test(
      path,
    )
  );
}

async function discoverJsonFilesInDirectory(root, directory) {
  const safeDirectory = assertContained(root, directory);
  const entries = await readdir(safeDirectory, { withFileTypes: true });
  const files = [];
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    const absolutePath = assertContained(root, resolve(safeDirectory, entry.name));
    const stat = await lstat(absolutePath);
    if (stat.isSymbolicLink()) {
      throw new Error(`Protocol source must not contain symlinks: ${absolutePath}`);
    }
    if (stat.isDirectory()) {
      files.push(...(await discoverJsonFilesInDirectory(root, absolutePath)));
    } else if (stat.isFile()) {
      const path = relative(resolve(root), absolutePath).split(sep).join("/");
      if (matchesSourcePath(path)) files.push(path);
    }
  }
  return files.sort();
}

export async function discoverJsonFiles(root) {
  const files = [];
  for (const directory of [
    "openapi",
    "events",
    "schemas",
    "examples",
    "fixtures",
    "compatibility",
    "conformance/cases",
    "profiles",
  ]) {
    const absolutePath = assertContained(root, resolve(root, directory));
    let stat;
    try {
      stat = await lstat(absolutePath);
    } catch (error) {
      if (directory === "profiles" && error?.code === "ENOENT") continue;
      throw error;
    }
    if (!stat.isDirectory() || stat.isSymbolicLink()) {
      throw new Error(`Protocol source directory must be a real directory: ${absolutePath}`);
    }
    files.push(...(await discoverJsonFilesInDirectory(root, absolutePath)));
  }
  return files.sort();
}

export async function sha256File(path) {
  return createHash("sha256")
    .update(await readFile(path))
    .digest("hex");
}

export function sha256DigestMap(entries) {
  const hash = createHash("sha256");
  for (const [path, digest] of Object.entries(entries).sort(([left], [right]) =>
    left.localeCompare(right),
  )) {
    hash.update(path);
    hash.update("\0");
    hash.update(digest);
    hash.update("\n");
  }
  return hash.digest("hex");
}

export function candidateSourceIdentity(repository, baseCommit, entries) {
  if (
    typeof repository !== "string" ||
    repository.length === 0 ||
    !/^[0-9a-f]{40}$/.test(baseCommit)
  ) {
    throw new Error("Candidate protocol source identity is invalid");
  }
  return {
    kind: "local-content",
    repository,
    baseCommit,
    contentSha256: sha256DigestMap(entries),
  };
}

export function filesForProfile(entries, profile) {
  return Object.fromEntries(
    Object.entries(entries).filter(([path]) => {
      const isCurrentHub = path.startsWith("profiles/hub-http-v1/");
      return profile === "currentHub" ? isCurrentHub : !isCurrentHub;
    }),
  );
}

export async function readProtocolLock(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

export function stableJson(value) {
  return `${JSON.stringify(sortJson(value), null, 2)}\n`;
}

function sortJson(value) {
  if (Array.isArray(value)) return value.map(sortJson);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value)
        .sort(([left], [right]) => left.localeCompare(right))
        .map(([key, child]) => [key, sortJson(child)]),
    );
  }
  return value;
}
