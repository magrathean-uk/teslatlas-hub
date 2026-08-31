import { execFileSync } from "node:child_process";
import { cp, lstat, mkdir, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import {
  assertContained,
  currentProfile,
  discoverJsonFiles,
  protocolCommit,
  sha256File,
  stableJson,
  supportedProfiles,
} from "./protocol-files.mjs";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const sourceRoot = join(repositoryRoot, "protocol/source");
const lockPath = join(repositoryRoot, "protocol/lock.json");
const generatedPaths = ["src/generated/protocol.ts", "src/generated/validators.ts", "src/generated/protocol-cases.ts"];
const checkout = process.argv[2];

if (typeof checkout !== "string" || !checkout.startsWith("/")) {
  throw new Error("Usage: node scripts/sync-protocol.mjs /absolute/path/to/teslatlas-protocol");
}
const authorityRoot = resolve(checkout);
const authorityStat = await lstat(authorityRoot);
if (!authorityStat.isDirectory() || authorityStat.isSymbolicLink()) {
  throw new Error("Protocol checkout must be a real directory");
}
const authorityCommit = execFileSync("git", ["-C", authorityRoot, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
if (authorityCommit !== protocolCommit) {
  throw new Error(`Protocol checkout must be at ${protocolCommit}, got ${authorityCommit}`);
}

const files = await discoverJsonFiles(authorityRoot);
await rm(sourceRoot, { recursive: true, force: true });
for (const path of files) {
  const source = assertContained(authorityRoot, join(authorityRoot, path));
  const stat = await lstat(source);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`Protocol input must be a regular file: ${source}`);
  const destination = assertContained(sourceRoot, join(sourceRoot, path));
  await mkdir(dirname(destination), { recursive: true });
  await cp(source, destination, { dereference: false });
}

const lock = {
  commit: protocolCommit,
  currentProfile,
  files: Object.fromEntries(await Promise.all(files.map(async (path) => [path, await sha256File(join(sourceRoot, path))]))),
  generated: {},
  generator: { package: "openapi-typescript", version: "7.13.0" },
  supportedProfiles,
};
await mkdir(dirname(lockPath), { recursive: true });
await writeFile(lockPath, stableJson(lock));
execFileSync(process.execPath, ["scripts/generate-protocol.mjs"], { cwd: repositoryRoot, stdio: "inherit" });
lock.generated = Object.fromEntries(await Promise.all(generatedPaths.map(async (path) => [path, await sha256File(join(repositoryRoot, path))])));
await writeFile(lockPath, stableJson(lock));
