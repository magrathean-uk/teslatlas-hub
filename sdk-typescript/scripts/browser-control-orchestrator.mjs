import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, lstat, mkdir, readFile, writeFile } from "node:fs/promises";
import { isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  DEFAULT_CONTROL_PORTS,
  prepareBrowserControlFiles,
} from "./browser-control-preflight-template.mjs";
import { SEQUENTIAL_MACOS_TRUST_MODE } from "./browser-trust-sequence.mjs";

const DEFAULT_TIMEOUT_MS = 120_000;
const MAX_CAPTURE_BYTES = 4 * 1024 * 1024;
const OWNER_DIRECTORY_MODE = 0o700;
const OWNER_FILE_MODE = 0o600;

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function now() {
  return new Date().toISOString();
}

function serializeError(error) {
  return {
    name: error?.name ?? "Error",
    message: String(error?.message ?? error),
    code: error?.code ?? null,
    signal: error?.signal ?? null,
  };
}

function assertCanonicalAbsolute(value, label) {
  if (typeof value !== "string" || !isAbsolute(value) || resolve(value) !== value) {
    throw new Error(`${label} must be an absolute canonical path`);
  }
  return value;
}

function assertOwnedPath(controlRoot, value, label) {
  assertCanonicalAbsolute(value, label);
  const relativePath = relative(controlRoot, value);
  if (relativePath === "" || relativePath.startsWith(`..${"/"}`) || relativePath.includes("../")) {
    throw new Error(`${label} must be inside controlRoot`);
  }
  return value;
}

async function assertAbsent(path, label) {
  try {
    await lstat(path);
  } catch (error) {
    if (error?.code === "ENOENT") return;
    throw error;
  }
  throw new Error(`${label} must be absent before orchestration: ${path}`);
}

function normalizeCommand(spec, label) {
  if (spec === null || typeof spec !== "object" || Array.isArray(spec)) {
    throw new Error(`${label} command must be an object`);
  }
  if (typeof spec.command !== "string" || spec.command.length === 0) {
    throw new Error(`${label}.command must be a non-empty string`);
  }
  const args = spec.args ?? [];
  if (!Array.isArray(args) || args.some((arg) => typeof arg !== "string")) {
    throw new Error(`${label}.args must be an array of strings`);
  }
  const env = spec.env ?? {};
  if (
    env === null ||
    typeof env !== "object" ||
    Array.isArray(env) ||
    Object.entries(env).some(([key, value]) => key.length === 0 || typeof value !== "string")
  ) {
    throw new Error(`${label}.env must be a string map`);
  }
  const cwd =
    spec.cwd === undefined ? process.cwd() : assertCanonicalAbsolute(spec.cwd, `${label}.cwd`);
  return {
    name: typeof spec.name === "string" && spec.name.length > 0 ? spec.name : label,
    command: spec.command,
    args: [...args],
    cwd,
    env: { ...env },
  };
}

function normalizeConfig(config) {
  if (config === null || typeof config !== "object" || Array.isArray(config)) {
    throw new Error("orchestrator config must be an object");
  }
  const controlRoot = assertCanonicalAbsolute(config.controlRoot, "controlRoot");
  const ports = config.ports ?? DEFAULT_CONTROL_PORTS;
  if (!Array.isArray(ports) || ports.some((port) => !Number.isSafeInteger(port) || port < 1)) {
    throw new Error("ports must be an array of positive integers");
  }
  const timeoutMs = config.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) {
    throw new Error("timeoutMs must be a positive integer");
  }
  const supervisor = normalizeCommand(config.supervisor, "supervisor");
  const readyStatePath = assertOwnedPath(
    controlRoot,
    config.supervisor.readyStatePath ?? join(controlRoot, "supervisor-state.json"),
    "supervisor.readyStatePath",
  );
  const readyStatus = config.supervisor.readyStatus ?? "serving";
  if (typeof readyStatus !== "string" || readyStatus.length === 0) {
    throw new Error("supervisor.readyStatus must be a non-empty string");
  }
  const controls = config.controls ?? [];
  if (!Array.isArray(controls)) throw new Error("controls must be an array");
  const cleanup = config.cleanup ?? [];
  if (!Array.isArray(cleanup) || cleanup.length === 0) {
    throw new Error("cleanup must contain at least one command");
  }
  const runner = config.runner === null ? null : normalizeCommand(config.runner, "runner");
  const trustMode = config.trustMode ?? SEQUENTIAL_MACOS_TRUST_MODE;
  if (trustMode !== SEQUENTIAL_MACOS_TRUST_MODE) {
    throw new Error(`orchestrator requires ${SEQUENTIAL_MACOS_TRUST_MODE}`);
  }
  if (
    runner !== null &&
    runner.env.TESLATLAS_BROWSER_TRUST_MODE !== undefined &&
    runner.env.TESLATLAS_BROWSER_TRUST_MODE !== trustMode
  ) {
    throw new Error("runner trust mode does not match the sequential orchestration mode");
  }
  const boundRunner =
    runner === null
      ? null
      : {
          ...runner,
          env: { ...runner.env, TESLATLAS_BROWSER_TRUST_MODE: trustMode },
        };
  const normalizedReceiptPath =
    config.receiptPath === undefined
      ? undefined
      : assertOwnedPath(controlRoot, config.receiptPath, "receiptPath");
  return {
    controlRoot,
    ports: [...ports],
    timeoutMs,
    supervisor: { ...supervisor, readyStatePath, readyStatus },
    controls: controls.map((spec, index) => normalizeCommand(spec, `control-${index + 1}`)),
    runner: boundRunner,
    cleanup: cleanup.map((spec, index) => normalizeCommand(spec, `cleanup-${index + 1}`)),
    receiptPath: normalizedReceiptPath,
    trustMode,
  };
}

function orchestrationCapturePath(controlRoot) {
  return join(controlRoot, "evidence", "orchestration");
}

async function assertCaptureDirectoryAbsent(controlRoot) {
  const captureRoot = orchestrationCapturePath(controlRoot);
  await assertAbsent(captureRoot, "orchestration capture directory");
  return captureRoot;
}

async function prepareCaptureDirectory(captureRoot) {
  await assertAbsent(captureRoot, "orchestration capture directory");
  await mkdir(captureRoot, { mode: OWNER_DIRECTORY_MODE });
  await chmod(captureRoot, OWNER_DIRECTORY_MODE);
  return captureRoot;
}

function appendOutput(record, field, chunk) {
  const text = String(chunk);
  const bytes = Buffer.from(text);
  const byteField = `${field}Bytes`;
  const chunksField = `${field}Chunks`;
  const truncatedField = `${field}Truncated`;
  const remaining = MAX_CAPTURE_BYTES - record[byteField];
  if (remaining <= 0) {
    record[truncatedField] = true;
    return;
  }
  const retained = bytes.subarray(0, remaining).toString("utf8");
  record[chunksField].push(retained);
  record[byteField] += Buffer.byteLength(retained, "utf8");
  if (bytes.length > remaining) record[truncatedField] = true;
}

function spawnTracked(spec, sequence) {
  const child = spawn(spec.command, spec.args, {
    cwd: spec.cwd,
    detached: false,
    env: { ...process.env, ...spec.env },
    shell: false,
    stdio: ["ignore", "pipe", "pipe"],
  });
  const record = {
    name: spec.name,
    command: spec.command,
    args: [...spec.args],
    cwd: spec.cwd,
    pid: child.pid ?? null,
    parentPid: process.pid,
    detached: false,
    startedAt: now(),
    endedAt: undefined,
    events: [{ at: now(), event: "spawn", pid: child.pid ?? null }],
    stdoutChunks: [],
    stderrChunks: [],
    stdoutBytes: 0,
    stderrBytes: 0,
    stdoutTruncated: false,
    stderrTruncated: false,
    child,
    error: null,
    exit: undefined,
    close: undefined,
    signalObserved: null,
    closed: false,
    forcedSignals: [],
    captureError: null,
    stdoutPath: null,
    stderrPath: null,
    sequence,
  };
  child.stdout?.on("data", (chunk) => appendOutput(record, "stdout", chunk));
  child.stderr?.on("data", (chunk) => appendOutput(record, "stderr", chunk));
  child.once("error", (error) => {
    record.error = serializeError(error);
    record.events.push({ at: now(), event: "error", error: record.error });
  });
  child.once("exit", (code, signal) => {
    record.exit = { code, signal };
    record.signalObserved = signal;
    record.events.push({ at: now(), event: "exit", code, signal });
    if (signal !== null) record.events.push({ at: now(), event: "signal", signal });
  });
  record.closedPromise = new Promise((resolveClose) => {
    child.once("close", (code, signal) => {
      record.close = { code, signal };
      record.closed = true;
      record.endedAt = now();
      record.events.push({ at: now(), event: "close", code, signal });
      resolveClose();
    });
  });
  return record;
}

async function waitForClose(record, timeoutMs, label = `${record.name} close`) {
  if (record.closed) return;
  let timer;
  try {
    await Promise.race([
      record.closedPromise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`timed out waiting for ${label}`)), timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function stopTracked(record, timeoutMs) {
  if (record.closed) return;
  record.forcedSignals.push("SIGTERM");
  record.events.push({ at: now(), event: "forced_stop_requested", signal: "SIGTERM" });
  record.child.kill("SIGTERM");
  try {
    await waitForClose(record, timeoutMs, `${record.name} SIGTERM close`);
  } catch {
    record.forcedSignals.push("SIGKILL");
    record.events.push({ at: now(), event: "forced_stop_requested", signal: "SIGKILL" });
    record.child.kill("SIGKILL");
    await waitForClose(record, timeoutMs, `${record.name} SIGKILL close`);
  }
}

function safeOutputName(record) {
  const label = record.name.replace(/[^A-Za-z0-9._-]/gu, "_") || "child";
  return `${String(record.sequence).padStart(2, "0")}-${label}`;
}

async function persistOutput(record, captureRoot) {
  if (record.stdoutPath !== null) return;
  const base = safeOutputName(record);
  const stdoutPath = join(captureRoot, `${base}.stdout`);
  const stderrPath = join(captureRoot, `${base}.stderr`);
  try {
    const stdout = record.stdoutChunks.join("");
    const stderr = record.stderrChunks.join("");
    await writeFile(stdoutPath, stdout, { flag: "wx", mode: OWNER_FILE_MODE });
    await writeFile(stderrPath, stderr, { flag: "wx", mode: OWNER_FILE_MODE });
    await chmod(stdoutPath, OWNER_FILE_MODE);
    await chmod(stderrPath, OWNER_FILE_MODE);
    record.stdoutPath = stdoutPath;
    record.stderrPath = stderrPath;
    record.stdoutSha256 = sha256(stdout);
    record.stderrSha256 = sha256(stderr);
  } catch (error) {
    record.captureError = serializeError(error);
  }
}

async function persistRunnerOutputs(preflight, record) {
  if (record === null || preflight?.runnerOutputPaths?.length !== 2) return;
  try {
    const [stdoutPath, stderrPath] = preflight.runnerOutputPaths;
    await writeFile(stdoutPath, record.stdoutChunks.join(""), { flag: "r+" });
    await writeFile(stderrPath, record.stderrChunks.join(""), { flag: "r+" });
    await chmod(stdoutPath, OWNER_FILE_MODE);
    await chmod(stderrPath, OWNER_FILE_MODE);
  } catch (error) {
    record.captureError = serializeError(error);
  }
}

function publicRecord(record) {
  return {
    name: record.name,
    command: record.command,
    args: record.args,
    cwd: record.cwd,
    pid: record.pid,
    parentPid: record.parentPid,
    detached: record.detached,
    startedAt: record.startedAt,
    endedAt: record.endedAt,
    events: record.events,
    error: record.error,
    exit: record.exit ?? null,
    close: record.close ?? null,
    signalObserved: record.signalObserved,
    forcedSignals: [...record.forcedSignals],
    stdoutPath: record.stdoutPath,
    stderrPath: record.stderrPath,
    stdoutSha256: record.stdoutSha256 ?? null,
    stderrSha256: record.stderrSha256 ?? null,
    stdoutBytes: record.stdoutBytes,
    stderrBytes: record.stderrBytes,
    stdoutTruncated: record.stdoutTruncated,
    stderrTruncated: record.stderrTruncated,
    captureError: record.captureError,
  };
}

function recordSucceeded(record) {
  return (
    record !== null &&
    record.error === null &&
    record.captureError === null &&
    record.exit?.code === 0 &&
    record.exit?.signal === null &&
    record.close?.code === 0 &&
    record.close?.signal === null
  );
}

function recordFailure(phase, record) {
  if (recordSucceeded(record)) return null;
  return {
    phase,
    message: `${phase} command did not complete successfully`,
    command: record ? record.name : null,
    error: record?.error ?? record?.captureError ?? null,
    exit: record?.exit ?? null,
    close: record?.close ?? null,
    signal: record?.signalObserved ?? null,
  };
}

async function runCommand(spec, sequence, captureRoot, timeoutMs) {
  const record = spawnTracked(spec, sequence);
  try {
    await waitForClose(record, timeoutMs);
  } catch (error) {
    record.error ??= serializeError(error);
    record.events.push({ at: now(), event: "timeout", error: serializeError(error) });
    await stopTracked(record, timeoutMs);
  }
  await persistOutput(record, captureRoot);
  return record;
}

async function waitForReady(record, statePath, expectedStatus, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastStateError;
  while (Date.now() < deadline) {
    if (record.closed) {
      throw new Error(
        `supervisor closed before ready: ${JSON.stringify({ exit: record.exit, close: record.close })}`,
      );
    }
    try {
      const state = JSON.parse(await readFile(statePath, "utf8"));
      if (state?.status === expectedStatus) return state;
      lastStateError = new Error(`supervisor state was ${state?.status ?? "missing status"}`);
    } catch (error) {
      if (error?.code !== "ENOENT" && error instanceof SyntaxError === false)
        lastStateError = error;
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 50));
  }
  throw new Error(`supervisor did not become ready: ${lastStateError?.message ?? "timeout"}`);
}

function publicChildren(children) {
  return {
    supervisor: children.supervisor ? publicRecord(children.supervisor) : null,
    controls: children.controls.map(publicRecord),
    runner: children.runner ? publicRecord(children.runner) : null,
    cleanup: children.cleanup.map(publicRecord),
  };
}

export async function runOwnedBrowserControl(config) {
  let normalized;
  let captureRoot;
  let preflight = null;
  let primaryFailure = null;
  let receiptPathFreshnessChecked = false;
  const cleanupFailures = [];
  const children = { supervisor: null, controls: [], runner: null, cleanup: [] };
  const cleanupAttempts = [];
  let sequence = 0;
  let supervisorReady = false;

  try {
    normalized = normalizeConfig(config);
    if (normalized.receiptPath !== undefined) {
      await assertAbsent(normalized.receiptPath, "receiptPath");
    }
    receiptPathFreshnessChecked = true;
    const plannedCaptureRoot = await assertCaptureDirectoryAbsent(normalized.controlRoot);
    preflight = await prepareBrowserControlFiles({
      controlRoot: normalized.controlRoot,
      ports: normalized.ports,
    });
    captureRoot = await prepareCaptureDirectory(plannedCaptureRoot);

    children.supervisor = spawnTracked(normalized.supervisor, sequence++);
    try {
      await waitForReady(
        children.supervisor,
        normalized.supervisor.readyStatePath,
        normalized.supervisor.readyStatus,
        normalized.timeoutMs,
      );
      supervisorReady = true;
    } catch (error) {
      primaryFailure = { phase: "supervisor-ready", error: serializeError(error) };
    }

    if (primaryFailure === null) {
      for (const control of normalized.controls) {
        const record = await runCommand(control, sequence++, captureRoot, normalized.timeoutMs);
        children.controls.push(record);
        const failure = recordFailure(`control:${control.name}`, record);
        if (failure !== null) {
          primaryFailure = failure;
          break;
        }
      }
    }

    if (primaryFailure === null && normalized.runner !== null) {
      children.runner = await runCommand(
        normalized.runner,
        sequence++,
        captureRoot,
        normalized.timeoutMs,
      );
      await persistRunnerOutputs(preflight, children.runner);
      primaryFailure = recordFailure("sdk-runner", children.runner);
    }
  } catch (error) {
    primaryFailure ??= { phase: "orchestrator", error: serializeError(error) };
  } finally {
    if (children.supervisor !== null && normalized !== undefined) {
      for (const cleanup of normalized.cleanup) {
        const cleanupSequence = sequence++;
        const attempt = {
          name: cleanup.name,
          sequence: cleanupSequence,
          status: "started",
          record: null,
          error: null,
          failure: null,
        };
        cleanupAttempts.push(attempt);
        try {
          const record = await runCommand(
            cleanup,
            cleanupSequence,
            captureRoot,
            normalized.timeoutMs,
          );
          attempt.record = record;
          children.cleanup.push(record);
          const failure = recordFailure(`cleanup:${cleanup.name}`, record);
          attempt.status = failure === null ? "passed" : "failed";
          attempt.failure = failure;
          if (failure !== null) cleanupFailures.push(failure);
        } catch (error) {
          attempt.status = "failed";
          attempt.error = serializeError(error);
          const failure = {
            phase: `cleanup:${cleanup.name}`,
            message: "cleanup command could not be completed",
            command: cleanup.name,
            attempted: true,
            error: attempt.error,
          };
          attempt.failure = failure;
          cleanupFailures.push(failure);
        }
      }
      try {
        await waitForClose(children.supervisor, normalized.timeoutMs, "supervisor shutdown");
      } catch {
        await stopTracked(children.supervisor, normalized.timeoutMs);
        cleanupFailures.push({
          phase: "supervisor-stop",
          message: "supervisor required a forced stop after cleanup",
          signals: [...children.supervisor.forcedSignals],
        });
      }
    }
    if (children.supervisor !== null && captureRoot !== undefined) {
      await persistOutput(children.supervisor, captureRoot);
    }
  }

  if (children.supervisor !== null && !children.supervisor.closed) {
    cleanupFailures.push({
      phase: "supervisor-close",
      message: "supervisor remained alive after orchestration",
    });
  }
  if (cleanupFailures.length > 0 && primaryFailure === null) {
    primaryFailure = cleanupFailures[0];
  }
  const result = {
    schemaVersion: 1,
    status: primaryFailure === null ? "passed" : "failed",
    orchestratorPid: process.pid,
    controlRoot: normalized?.controlRoot ?? null,
    captureRoot: captureRoot ?? null,
    preflight,
    supervisorReady,
    children: publicChildren(children),
    cleanup: {
      commandsConfigured: normalized?.cleanup.length ?? 0,
      commandsAttempted: cleanupAttempts.length,
      attempts: cleanupAttempts.map((attempt) => ({
        name: attempt.name,
        sequence: attempt.sequence,
        status: attempt.status,
        record: attempt.record === null ? null : publicRecord(attempt.record),
        error: attempt.error,
        failure: attempt.failure,
      })),
      supervisorClosed: children.supervisor?.closed === true,
      forcedSupervisorStop: (children.supervisor?.forcedSignals.length ?? 0) > 0,
      failures: cleanupFailures,
    },
    failure: primaryFailure,
    browserStarted: false,
    hubContacted: false,
    keychainTouched: false,
    freshLiveFixture: false,
    trustMode: normalized?.trustMode ?? SEQUENTIAL_MACOS_TRUST_MODE,
  };

  if (normalized?.receiptPath !== undefined && receiptPathFreshnessChecked) {
    try {
      await writeFile(normalized.receiptPath, `${JSON.stringify(result, null, 2)}\n`, {
        flag: "wx",
        mode: OWNER_FILE_MODE,
      });
      await chmod(normalized.receiptPath, OWNER_FILE_MODE);
    } catch (error) {
      result.status = "failed";
      result.failure ??= { phase: "receipt", error: serializeError(error) };
    }
  }
  return result;
}

async function main() {
  const configPath = process.argv[2];
  if (!configPath) throw new Error("usage: node browser-control-orchestrator.mjs <config.json>");
  const config = JSON.parse(await readFile(configPath, "utf8"));
  const result = await runOwnedBrowserControl(config);
  console.log(JSON.stringify(result));
  if (result.status !== "passed") process.exitCode = 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(JSON.stringify({ status: "failed", error: String(error?.stack ?? error) }));
    process.exitCode = 1;
  });
}
