import { execFile } from "node:child_process";
import { chmod, lstat, mkdir, readdir, stat, writeFile } from "node:fs/promises";
import { isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

export const DEFAULT_CONTROL_PORTS = Object.freeze([4176, 9236, 9237]);
export const RUNNER_OUTPUT_NAMES = Object.freeze(["runner.stdout", "runner.stderr"]);
const OWNER_DIRECTORY_MODE = 0o700;
const OWNER_FILE_MODE = 0o600;

export function runtimeArtifactPaths(controlRoot) {
  const evidenceRoot = join(controlRoot, "evidence");
  return [
    join(controlRoot, "control.sock"),
    join(controlRoot, "supervisor-state.json"),
    join(evidenceRoot, "browser-control-events.jsonl"),
    join(controlRoot, "untrusted-profile"),
    join(controlRoot, "trusted-profile"),
    join(evidenceRoot, "imported-hub-ca.pem"),
    join(evidenceRoot, "browser-trust-witness.json"),
    join(evidenceRoot, "browser-acceptance-receipt.json"),
    ...RUNNER_OUTPUT_NAMES.map((name) => join(evidenceRoot, name)),
  ];
}

async function assertFreshRuntime(controlRoot) {
  for (const path of runtimeArtifactPaths(controlRoot)) {
    try {
      await lstat(path);
    } catch (error) {
      if (error?.code === "ENOENT") continue;
      throw error;
    }
    throw new Error(`preflight runtime artifact must be absent: ${path}`);
  }
}

async function listenerSnapshot(port) {
  try {
    const result = await execFileAsync("/usr/sbin/lsof", ["-nP", `-iTCP:${port}`, "-sTCP:LISTEN"], {
      encoding: "utf8",
      maxBuffer: 1024 * 1024,
      shell: false,
    });
    return { code: 0, stdout: result.stdout, stderr: result.stderr };
  } catch (error) {
    return {
      code:
        typeof error?.status === "number"
          ? error.status
          : typeof error?.code === "number"
            ? error.code
            : null,
      stdout: typeof error?.stdout === "string" ? error.stdout : "",
      stderr: typeof error?.stderr === "string" ? error.stderr : String(error?.message ?? error),
    };
  }
}

async function assertPortClosed(port) {
  const result = await listenerSnapshot(port);
  if ((result.code === 0 || result.code === 1) && result.stdout.trim() === "") return;
  throw new Error(`preflight port ${port} is not closed: ${result.stdout.trim() || result.stderr}`);
}

async function directoryPaths(root) {
  const paths = [root];
  for (const entry of await readdir(root, { withFileTypes: true })) {
    if (entry.isSymbolicLink()) {
      throw new Error(`preflight symlink is not allowed: ${join(root, entry.name)}`);
    }
    if (!entry.isDirectory()) continue;
    paths.push(...(await directoryPaths(join(root, entry.name))));
  }
  return paths;
}

async function enforceDirectoryModes(controlRoot) {
  const paths = await directoryPaths(controlRoot);
  for (const path of paths) await chmod(path, OWNER_DIRECTORY_MODE);
  for (const path of paths) {
    if (((await stat(path)).mode & 0o777) !== OWNER_DIRECTORY_MODE) {
      throw new Error(`preflight directory is not owner-only searchable: ${path}`);
    }
  }
  return paths;
}

async function createOutputFiles(evidenceRoot) {
  const paths = RUNNER_OUTPUT_NAMES.map((name) => join(evidenceRoot, name));
  for (const path of paths) {
    await writeFile(path, "", { flag: "wx", mode: OWNER_FILE_MODE });
    await chmod(path, OWNER_FILE_MODE);
    const file = await stat(path);
    if (!file.isFile() || (file.mode & 0o777) !== OWNER_FILE_MODE) {
      throw new Error(`preflight runner output is not an owner-only regular file: ${path}`);
    }
  }
  return paths;
}

async function enforceExistingSensitiveFileModes(controlRoot, evidenceRoot) {
  const paths = [
    join(controlRoot, "preparation.json"),
    join(evidenceRoot, "runner.stdout"),
    join(evidenceRoot, "runner.stderr"),
  ];
  for (const path of paths) {
    try {
      await chmod(path, OWNER_FILE_MODE);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
  }
}

export async function prepareBrowserControlFiles({ controlRoot, ports = DEFAULT_CONTROL_PORTS }) {
  if (
    typeof controlRoot !== "string" ||
    !isAbsolute(controlRoot) ||
    resolve(controlRoot) !== controlRoot
  ) {
    throw new Error("controlRoot must be an absolute canonical path");
  }
  if (!Array.isArray(ports) || ports.some((port) => !Number.isSafeInteger(port) || port < 1)) {
    throw new Error("ports must be positive integers");
  }
  const root = await lstat(controlRoot);
  if (root.isSymbolicLink() || !root.isDirectory()) {
    throw new Error("controlRoot must be a real directory");
  }

  await assertFreshRuntime(controlRoot);
  for (const port of ports) await assertPortClosed(port);

  const evidenceRoot = join(controlRoot, "evidence");
  await mkdir(evidenceRoot, { recursive: true, mode: OWNER_DIRECTORY_MODE });
  const directories = await enforceDirectoryModes(controlRoot);
  await enforceExistingSensitiveFileModes(controlRoot, evidenceRoot);
  const runnerOutputPaths = await createOutputFiles(evidenceRoot);

  return {
    status: "passed",
    browserStarted: false,
    hubContacted: false,
    keychainTouched: false,
    controlSocketAbsent: true,
    closedPorts: [...ports],
    directoryCount: directories.length,
    runnerOutputFiles: [...RUNNER_OUTPUT_NAMES],
    runnerOutputPaths,
    directoryMode: "0700",
    sensitiveFileMode: "0600",
  };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const controlRoot = process.env.TESLATLAS_BROWSER_CONTROL_ROOT;
  prepareBrowserControlFiles({ controlRoot })
    .then((result) => console.log(JSON.stringify({ ...result, runnerOutputPaths: undefined })))
    .catch((error) => {
      console.error(JSON.stringify({ status: "failed", error: String(error?.stack ?? error) }));
      process.exitCode = 1;
    });
}
