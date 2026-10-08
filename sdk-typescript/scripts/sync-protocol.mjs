import { execFileSync } from "node:child_process";
import {
  cp,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  realpath,
  rename,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, join, relative, resolve, sep } from "node:path";
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
const richerGeneratedPaths = [
  "src/generated/protocol.ts",
  "src/generated/validators.ts",
  "src/generated/protocol-cases.ts",
  "src/generated/event-catalog.ts",
];
const currentHubGeneratedPaths = [
  "src/generated/hub-protocol.ts",
  "src/generated/hub-validators.js",
  "src/generated/hub-validators.d.ts",
];
const generatedPaths = [...richerGeneratedPaths, ...currentHubGeneratedPaths];
const replacementPaths = ["protocol/source", ...generatedPaths, "protocol/lock.json"];
const transactionPath = (root) => join(root, "protocol/.sync-transaction.json");

// Maintainer-only boundary: disposable-tree tests need neither Git nor changes
// to the SDK's locked inputs. The CLI supplies the actual authority identity.
export async function synchronizeProtocol({
  root = repositoryRoot,
  checkout,
  candidateMode = false,
  authorityCommit,
  committedInputs,
  generate = generateStagedProtocol,
  failpoint = () => undefined,
}) {
  root = resolve(root);
  await requireRealDirectory(root, "SDK root");
  await recoverProtocolSync(root);
  const existingLock = await readProtocolLock(join(root, "protocol/lock.json"));
  const authorityRoot = resolve(checkout);
  await requireRealDirectory(authorityRoot, "Protocol checkout");
  if (
    !candidateMode &&
    (existingLock.source?.kind !== "git-commit" || authorityCommit !== existingLock.source?.commit)
  ) {
    throw new Error("Protocol checkout must be at the manifest-pinned commit");
  }
  const files = candidateMode
    ? await discoverJsonFiles(authorityRoot)
    : [...committedInputs.keys()].sort();
  const stageRoot = await mkdtemp(join(await realpath(tmpdir()), "teslatlas-protocol-sync-"));
  try {
    if ((await lstat(stageRoot)).dev !== (await lstat(root)).dev) {
      throw new Error(
        "Protocol staging and SDK must share a filesystem for recoverable replacement",
      );
    }
    const sourceRoot = join(stageRoot, "protocol/source");
    await mkdir(sourceRoot, { recursive: true });
    for (const path of files) {
      if (!matchesSourcePath(path)) throw new Error(`Unsupported Protocol input: ${path}`);
      const destination = assertContained(sourceRoot, join(sourceRoot, path));
      await mkdir(dirname(destination), { recursive: true });
      if (candidateMode) {
        const source = assertContained(authorityRoot, join(authorityRoot, path));
        await requireRegularFile(source, "Protocol input");
        await cp(source, destination, { dereference: false });
      } else {
        await writeFile(destination, committedInputs.get(path));
      }
    }
    await failpoint("inputs-staged", stageRoot);
    const lock = {
      files: Object.fromEntries(
        await Promise.all(
          files.map(async (path) => [path, await sha256File(join(sourceRoot, path))]),
        ),
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
    await writeFile(join(stageRoot, "protocol/lock.json"), stableJson(lock));
    await failpoint("initial-lock-staged", stageRoot);
    await generate(stageRoot);
    await failpoint("generation-complete", stageRoot);
    lock.generated = Object.fromEntries(
      await Promise.all(
        generatedPaths.map(async (path) => {
          const output = join(stageRoot, path);
          await requireRegularFile(output, "Generated Protocol output");
          return [path, await sha256File(output)];
        }),
      ),
    );
    lock.profiles.richer.generatedOutputSha256 = sha256DigestMap(
      Object.fromEntries(richerGeneratedPaths.map((path) => [path, lock.generated[path]])),
    );
    lock.profiles.currentHub.generatedOutputSha256 = sha256DigestMap(
      Object.fromEntries(currentHubGeneratedPaths.map((path) => [path, lock.generated[path]])),
    );
    await writeFile(join(stageRoot, "protocol/lock.json"), stableJson(lock));
    await failpoint("final-lock-staged", stageRoot);
    await replaceStagedProtocol(root, stageRoot, failpoint);
    return lock;
  } finally {
    // A failed rollback retains its journal and backup for explicit recovery.
    let retainStage = false;
    if (await pathExists(transactionPath(root))) {
      retainStage =
        JSON.parse(await readFile(transactionPath(root), "utf8")).stageRoot === stageRoot;
    }
    if (!retainStage) await rm(stageRoot, { recursive: true, force: true });
  }
}

function generateStagedProtocol(stageRoot) {
  execFileSync(
    process.execPath,
    [join(repositoryRoot, "scripts/generate-protocol.mjs"), "--output-root", stageRoot],
    { cwd: repositoryRoot, stdio: "inherit" },
  );
}

async function replaceStagedProtocol(root, stageRoot, failpoint) {
  const entries = [];
  for (const path of replacementPaths) {
    const target = join(root, path);
    await ensureReplacementParent(root, dirname(target));
    const originallyPresent = await pathExists(target);
    if (originallyPresent) {
      if (path === "protocol/source") await requireRealDirectory(target, "Protocol source");
      else await requireRegularFile(target, "Protocol replacement");
    }
    entries.push({ path, originallyPresent });
  }
  const journal = {
    schemaVersion: 1,
    state: "prepared",
    ownerPid: process.pid,
    stageRoot,
    entries,
  };
  await writeFile(transactionPath(root), stableJson(journal), { flag: "wx", mode: 0o600 });
  try {
    for (const entry of entries) {
      const target = join(root, entry.path);
      const backup = join(stageRoot, ".backup", entry.path);
      if (entry.originallyPresent) {
        await mkdir(dirname(backup), { recursive: true });
        await rename(target, backup);
      }
      await rename(join(stageRoot, entry.path), target);
      await failpoint(`replaced:${entry.path}`, stageRoot);
    }
    journal.state = "committed";
    const committedJournal = `${transactionPath(root)}.next`;
    await writeFile(committedJournal, stableJson(journal), { flag: "wx", mode: 0o600 });
    await rename(committedJournal, transactionPath(root));
  } catch (error) {
    try {
      await recoverProtocolSync(root, process.pid, failpoint);
    } catch (recoveryError) {
      throw new AggregateError(
        [error, recoveryError],
        `Protocol sync rollback failed; recover using ${transactionPath(root)}`,
      );
    }
    throw error;
  }
  await recoverProtocolSync(root, process.pid);
}

export async function recoverProtocolSync(root, activeOwnerPid, failpoint = () => undefined) {
  root = resolve(root);
  await requireRealDirectory(root, "SDK root");
  const journalPath = transactionPath(root);
  if (!(await pathExists(journalPath))) return;
  await requireRegularFile(journalPath, "Protocol transaction journal");
  const journal = JSON.parse(await readFile(journalPath, "utf8"));
  const temporaryRoot = await realpath(tmpdir());
  if (
    journal.schemaVersion !== 1 ||
    !["prepared", "committed"].includes(journal.state) ||
    !Number.isSafeInteger(journal.ownerPid) ||
    journal.ownerPid < 1 ||
    typeof journal.stageRoot !== "string" ||
    dirname(journal.stageRoot) !== temporaryRoot ||
    !/^teslatlas-protocol-sync-[A-Za-z0-9]+$/u.test(
      journal.stageRoot.slice(temporaryRoot.length + 1),
    ) ||
    !Array.isArray(journal.entries) ||
    journal.entries.length !== replacementPaths.length ||
    journal.entries.some(
      (entry, index) =>
        entry.path !== replacementPaths[index] || typeof entry.originallyPresent !== "boolean",
    )
  ) {
    throw new Error("Protocol transaction journal is invalid; current files were preserved");
  }
  const ownedRecovery = activeOwnerPid === process.pid && journal.ownerPid === process.pid;
  if (!ownedRecovery && processIsAlive(journal.ownerPid)) {
    throw new Error("Another Protocol synchronization owns the recovery journal");
  }
  if (journal.state === "prepared") {
    await requireRealDirectory(journal.stageRoot, "Protocol transaction staging");
    for (const entry of [...journal.entries].reverse()) {
      const backup = join(journal.stageRoot, ".backup", entry.path);
      const target = join(root, entry.path);
      if ((await realpath(dirname(target))) !== dirname(target)) {
        throw new Error(`Protocol recovery parent must not be a symlink: ${entry.path}`);
      }
      if (await pathExists(backup)) {
        if (entry.path === "protocol/source") await requireRealDirectory(backup, "Protocol backup");
        else await requireRegularFile(backup, "Protocol backup");
        await rm(target, { recursive: true, force: true });
        await rename(backup, target);
      } else if (
        !entry.originallyPresent &&
        !(await pathExists(join(journal.stageRoot, entry.path)))
      ) {
        await rm(target, { recursive: true, force: true });
      }
    }
  }
  await rm(`${journalPath}.next`, { force: true });
  // Restoration is complete before the recovery pointer is removed. Disposal
  // may fail or be interrupted without leaving a pointer to missing backups.
  await rm(journalPath);
  await failpoint("recovery-journal-removed", journal.stageRoot);
  await rm(journal.stageRoot, { recursive: true, force: true });
}

async function ensureReplacementParent(root, directory) {
  const parent = assertContained(root, directory);
  let current = root;
  for (const part of relative(root, parent).split(sep).filter(Boolean)) {
    current = join(current, part);
    if (!(await pathExists(current))) await mkdir(current);
    await requireRealDirectory(current, "Protocol replacement parent");
  }
}

async function pathExists(path) {
  try {
    await lstat(path);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

function processIsAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH") return false;
    throw error;
  }
}

async function requireRealDirectory(path, label) {
  const stat = await lstat(path);
  if (!stat.isDirectory() || stat.isSymbolicLink() || (await realpath(path)) !== path) {
    throw new Error(`${label} must be a real directory without symlink parents`);
  }
}

async function requireRegularFile(path, label) {
  const stat = await lstat(path);
  if (!stat.isFile() || stat.isSymbolicLink() || (await realpath(path)) !== path) {
    throw new Error(`${label} must be a regular file without symlink parents: ${path}`);
  }
}

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

async function main(values) {
  const candidateMode = values[0] === "--candidate";
  const checkout = values[candidateMode ? 1 : 0];
  if (
    typeof checkout !== "string" ||
    !checkout.startsWith("/") ||
    values.length !== (candidateMode ? 2 : 1)
  ) {
    throw new Error(
      "Usage: node scripts/sync-protocol.mjs [--candidate] /absolute/path/to/teslatlas-protocol",
    );
  }
  const authorityRoot = resolve(checkout);
  await requireRealDirectory(authorityRoot, "Protocol checkout");
  const authorityCommit = execFileSync("git", ["-C", authorityRoot, "rev-parse", "HEAD"], {
    encoding: "utf8",
  }).trim();
  return synchronizeProtocol({
    checkout: authorityRoot,
    candidateMode,
    authorityCommit,
    committedInputs: candidateMode
      ? undefined
      : committedProtocolInputs(authorityRoot, authorityCommit),
  });
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`Protocol sync: ${error.message}\n`);
    process.exitCode = 1;
  });
}
