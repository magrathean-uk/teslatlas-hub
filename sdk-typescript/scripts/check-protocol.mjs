import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { lstat, mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
// The only contract input: the in-repo Protocol profile the generator reads in place.
const profileRoot = resolve(repositoryRoot, "../protocol/profiles/hub-http-v1/1.2.0");
const generatedRoot = join(repositoryRoot, "src/generated");
const generatedFiles = ["hub-protocol.ts", "hub-validators.d.ts", "hub-validators.js"];

async function sha256File(path) {
  return createHash("sha256")
    .update(await readFile(path))
    .digest("hex");
}

async function listFiles(root, directory = root) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    const stat = await lstat(path);
    if (stat.isSymbolicLink())
      throw new Error(`Protocol profile must not contain symlinks: ${path}`);
    if (stat.isDirectory()) files.push(...(await listFiles(root, path)));
    else if (stat.isFile()) files.push(relative(root, path).split(sep).join("/"));
    else throw new Error(`Protocol profile holds a non-regular entry: ${path}`);
  }
  return files.sort();
}

function parseSha256Sums(text) {
  if (!text.endsWith("\n")) throw new Error("SHA256SUMS must end with a newline");
  const listed = new Map();
  for (const line of text.slice(0, -1).split("\n")) {
    const match = /^([0-9a-f]{64}) {2}(\S.*)$/u.exec(line);
    if (match === null) throw new Error("SHA256SUMS holds a malformed line");
    const [, digest, path] = match;
    if (
      isAbsolute(path) ||
      path.includes("\\") ||
      path.split("/").some((part) => part === "" || part === "." || part === "..") ||
      path === "SHA256SUMS"
    ) {
      throw new Error(`SHA256SUMS lists an unsafe path: ${path}`);
    }
    if (listed.has(path)) throw new Error(`SHA256SUMS lists a path twice: ${path}`);
    listed.set(path, digest);
  }
  return listed;
}

// 1. The profile bytes are the ones its own SHA256SUMS records, and nothing else is in the directory.
const listed = parseSha256Sums(await readFile(join(profileRoot, "SHA256SUMS"), "utf8"));
const present = (await listFiles(profileRoot)).filter((path) => path !== "SHA256SUMS");
if (JSON.stringify([...listed.keys()].sort()) !== JSON.stringify(present)) {
  throw new Error("SHA256SUMS does not list exactly the files of the Protocol profile");
}
for (const [path, digest] of listed) {
  if ((await sha256File(join(profileRoot, path))) !== digest) {
    throw new Error(`Protocol profile hash mismatch: ${path}`);
  }
}
const profile = JSON.parse(await readFile(join(profileRoot, "profile.json"), "utf8"));
if (profile.profile_id !== "hub-http-v1@1.2.0" || profile.contract_version !== "1.2.0") {
  throw new Error("Protocol profile is not hub-http-v1@1.2.0");
}

// 2. The committed generated files are exactly what the generator emits from those bytes.
if (JSON.stringify((await readdir(generatedRoot)).sort()) !== JSON.stringify(generatedFiles)) {
  throw new Error("src/generated must hold exactly the generated Hub files");
}
const temporaryOutput = await mkdtemp(join(tmpdir(), "teslatlas-sdk-protocol-"));
try {
  execFileSync(process.execPath, ["scripts/generate-protocol.mjs"], {
    cwd: repositoryRoot,
    env: { ...process.env, TESLATLAS_PROTOCOL_OUTPUT_DIR: temporaryOutput },
    stdio: "pipe",
  });
  for (const file of generatedFiles) {
    const regenerated = await readFile(join(temporaryOutput, file));
    if (!regenerated.equals(await readFile(join(generatedRoot, file)))) {
      throw new Error(`Protocol generated output mismatch: src/generated/${file}`);
    }
  }
} finally {
  await rm(temporaryOutput, { recursive: true, force: true });
}
console.log(`Protocol hub-http-v1@1.2.0 verified: ${listed.size} files, generated output matches`);
