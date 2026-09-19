import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { lstat, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const CLOSE_TIMEOUT_MS = 10_000;
const START_TIMEOUT_MS = 5_000;
const CERTIFICATE_SHA256 = "0".repeat(64);
const SYNTHETIC_HUB_ENDPOINT = "https://localhost:1";

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function now() {
  return new Date().toISOString();
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

async function waitFor(predicate, description, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`timed out waiting for ${description}`);
}

async function reservePort() {
  const { createServer } = await import("node:net");
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const port = server.address().port;
  await new Promise((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
  });
  return port;
}

function spawnTracked(label, args, cwd) {
  const child = spawn(process.execPath, args, {
    cwd,
    detached: false,
    env: {
      PATH: process.env.PATH ?? "",
      TMPDIR: process.env.TMPDIR ?? tmpdir(),
    },
    shell: false,
    stdio: ["ignore", "pipe", "pipe"],
  });
  const record = {
    label,
    pid: child.pid ?? null,
    detached: false,
    parentPid: process.pid,
    child,
    stdout: "",
    stderr: "",
    events: [{ at: now(), event: "spawn", pid: child.pid ?? null }],
    exit: undefined,
    close: undefined,
    error: undefined,
    closed: false,
  };
  child.stdout?.setEncoding("utf8");
  child.stderr?.setEncoding("utf8");
  child.stdout?.on("data", (chunk) => {
    record.stdout += String(chunk);
  });
  child.stderr?.on("data", (chunk) => {
    record.stderr += String(chunk);
  });
  child.once("error", (error) => {
    record.error = { name: error.name, message: error.message, code: error.code ?? null };
    record.events.push({ at: now(), event: "error", error: record.error });
  });
  child.once("exit", (code, signal) => {
    record.exit = { code, signal };
    record.events.push({ at: now(), event: "exit", code, signal });
  });
  record.closedPromise = new Promise((resolve) => {
    child.once("close", (code, signal) => {
      record.close = { code, signal };
      record.closed = true;
      record.events.push({ at: now(), event: "close", code, signal });
      resolve();
    });
  });
  return record;
}

async function waitForClose(record, description = `${record.label} close`) {
  if (record.closed) return;
  let timer;
  try {
    await Promise.race([
      record.closedPromise,
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`timed out waiting for ${description}`)),
          CLOSE_TIMEOUT_MS,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function stopOwnedProcess(record) {
  if (!record || record.closed) return;
  record.child.kill("SIGTERM");
  try {
    await waitForClose(record);
  } catch {
    if (!record.closed) record.child.kill("SIGKILL");
    await waitForClose(record);
  }
}

function transformSupervisor(source, sourceRoot, controlRoot, trustedPort, untrustedPort) {
  let transformed = source;
  const replacements = [
    [JSON.stringify(sourceRoot), JSON.stringify(controlRoot)],
    [JSON.stringify("https://localhost:18529"), JSON.stringify(SYNTHETIC_HUB_ENDPOINT)],
    ["9238", String(trustedPort)],
    ["9239", String(untrustedPort)],
  ];
  for (const [from, to] of replacements) {
    if (!transformed.includes(from))
      throw new Error(`diagnostic replacement target missing: ${from}`);
    transformed = transformed.replaceAll(from, to);
  }
  return transformed;
}

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

async function main() {
  const sourcePath = process.argv[2];
  const controlSourcePath = process.argv[3];
  if (!sourcePath || !controlSourcePath) {
    throw new Error(
      "usage: node test-browser-supervisor-lifecycle.mjs <supervisor.mjs> <untrusted-control.mjs>",
    );
  }

  const [supervisorSource, controlSource] = await Promise.all([
    readFile(sourcePath, "utf8"),
    readFile(controlSourcePath, "utf8"),
  ]);
  const sourceRootMatch = supervisorSource.match(
    /export const CONTROL_ROOT =\n\s+(["'][^"']+["']);/u,
  );
  if (!sourceRootMatch) throw new Error("could not identify supervisor control root");
  const sourceRoot = JSON.parse(sourceRootMatch[1]);
  const canonicalTmpDir = await realpath("/tmp");
  const controlRoot = await mkdtemp(join(canonicalTmpDir, "tl-supervisor-"));
  const trustedPort = await reservePort();
  const untrustedPort = await reservePort();
  const supervisorPath = join(controlRoot, "supervisor.mjs");
  const controlPath = join(controlRoot, "untrusted-control.mjs");
  const socketPath = join(controlRoot, "control.sock");
  const statePath = join(controlRoot, "supervisor-state.json");
  const transformedSupervisor = transformSupervisor(
    supervisorSource,
    sourceRoot,
    controlRoot,
    trustedPort,
    untrustedPort,
  );
  await writeFile(supervisorPath, transformedSupervisor, { mode: 0o700 });
  await writeFile(controlPath, controlSource, { mode: 0o700 });

  let server;
  let client;
  try {
    server = spawnTracked("supervisor", [supervisorPath, "serve"], controlRoot);
    await waitFor(
      async () =>
        server.closed ||
        ((await pathExists(socketPath)) &&
          (await pathExists(statePath)) &&
          (await readJson(statePath)).status === "serving"),
      "supervisor serving state",
      START_TIMEOUT_MS,
    );
    if (server.closed) {
      throw new Error(
        `supervisor exited before control request: ${JSON.stringify({
          exit: server.exit,
          close: server.close,
          error: server.error,
          stdout: server.stdout,
          stderr: server.stderr,
        })}`,
      );
    }

    client = spawnTracked(
      "control-client",
      [
        controlPath,
        JSON.stringify({
          operation: "shutdown",
          attempt: 1,
          endpoint: SYNTHETIC_HUB_ENDPOINT,
          certificateSha256: CERTIFICATE_SHA256,
        }),
      ],
      controlRoot,
    );
    await waitForClose(client, "control client close");
    await waitForClose(server, "supervisor close");

    const stoppedState = await readJson(statePath);
    const clientResponse = JSON.parse(client.stdout.trim());
    assert.deepEqual(clientResponse, { observed: true, closed: true, portClosed: true });
    assert.deepEqual(client.exit, { code: 0, signal: null });
    assert.deepEqual(client.close, { code: 0, signal: null });
    assert.deepEqual(server.exit, { code: 0, signal: null });
    assert.deepEqual(server.close, { code: 0, signal: null });
    assert.equal(stoppedState.status, "stopped");
    assert.equal(await pathExists(socketPath), false);

    return {
      status: "passed",
      regression: "one-parent-supervisor-control-lifecycle",
      sourceSupervisorSha256: sha256(supervisorSource),
      transformedSupervisorSha256: sha256(transformedSupervisor),
      sourceControlRoot: sourceRoot,
      freshTemporaryRoot: true,
      freshTemporarySocket: true,
      orchestratorPid: process.pid,
      server: {
        pid: server.pid,
        parentPid: server.parentPid,
        detached: server.detached,
        exit: server.exit,
        close: server.close,
        signalObserved: server.exit?.signal ?? server.close?.signal ?? null,
        stdoutSha256: sha256(server.stdout),
        stderrSha256: sha256(server.stderr),
        events: server.events,
      },
      controlClient: {
        pid: client.pid,
        parentPid: client.parentPid,
        detached: client.detached,
        exit: client.exit,
        close: client.close,
        signalObserved: client.exit?.signal ?? client.close?.signal ?? null,
        response: clientResponse,
        stdoutSha256: sha256(client.stdout),
        stderrSha256: sha256(client.stderr),
        events: client.events,
      },
      stoppedState,
      cleanup: {
        socketRemoved: true,
        temporaryRootRemoved: false,
      },
      browserSpawned: false,
      hubContacted: false,
      keychainTouched: false,
    };
  } finally {
    await stopOwnedProcess(client);
    await stopOwnedProcess(server);
    await rm(controlRoot, { force: true, recursive: true });
  }
}

main()
  .then((result) => {
    result.cleanup.temporaryRootRemoved = true;
    console.log(JSON.stringify(result));
  })
  .catch((error) => {
    console.error(JSON.stringify({ status: "failed", error: String(error?.stack ?? error) }));
    process.exitCode = 1;
  });
