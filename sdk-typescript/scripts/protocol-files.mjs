import { createHash } from "node:crypto";
import { lstat, readdir, readFile } from "node:fs/promises";
import { relative, resolve, sep } from "node:path";

export const protocolCommit = "79ced4c7fdc79520ad31d72a0280bf5f3f19f407";
export const currentProfile = "1.2.0";
export const supportedProfiles = Object.freeze(["1.0.0", "1.1.0", "1.2.0"]);
export const sourceGlobs = Object.freeze([
  "openapi/teslatlas-v1.openapi.json",
  "events/teslatlas-v1.sse.json",
  "schemas/*.schema.json",
  "examples/**/*.json",
  "fixtures/**/*.json",
  "compatibility/**/*.json",
  "conformance/cases/*.json",
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
    /^conformance\/cases\/[^/]+\.json$/.test(path)
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
  for (const directory of ["openapi", "events", "schemas", "examples", "fixtures", "compatibility", "conformance/cases"]) {
    const absolutePath = assertContained(root, resolve(root, directory));
    const stat = await lstat(absolutePath);
    if (!stat.isDirectory() || stat.isSymbolicLink()) {
      throw new Error(`Protocol source directory must be a real directory: ${absolutePath}`);
    }
    files.push(...(await discoverJsonFilesInDirectory(root, absolutePath)));
  }
  return files.sort();
}

export async function sha256File(path) {
  return createHash("sha256").update(await readFile(path)).digest("hex");
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
