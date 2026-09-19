import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { lstat, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const NODE = process.execPath;
const ORCHESTRATOR = fileURLToPath(new URL("./browser-control-orchestrator.mjs", import.meta.url));

const FAKE_SUPERVISOR = `
import { lstat, writeFile } from "node:fs/promises";
import { join } from "node:path";

const [, , operation, root] = process.argv;
const statePath = join(root, "supervisor-state.json");
const shutdownPath = join(root, "shutdown.request");
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

if (operation === "fail") {
  console.error("synthetic supervisor stderr");
  process.exitCode = 12;
} else if (operation === "serve") {
  await writeFile(statePath, '{"status":"serving"}\\n', { mode: 0o600 });
  while (true) {
    try {
      await lstat(shutdownPath);
      break;
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
      await delay(10);
    }
  }
  await writeFile(statePath, '{"status":"stopped"}\\n', { mode: 0o600 });
  console.log("fake supervisor stopped");
} else {
  throw new Error("unsupported fake supervisor operation");
}
`;

const FAKE_CONTROL = `
import { writeFile } from "node:fs/promises";
import { join } from "node:path";

const [, , root, operation] = process.argv;
if (operation === "fail" || operation === "cleanup-failure") {
  if (operation === "cleanup-failure") {
    await writeFile(join(root, "cleanup-failure.attempted"), "observed\\n", { mode: 0o600 });
  }
  console.error("synthetic control stderr");
  process.exitCode = operation === "cleanup-failure" ? 11 : 9;
} else {
  const marker = operation === "shutdown" ? "shutdown.request" : "control-" + operation;
  await writeFile(join(root, marker), "observed\\n", { mode: 0o600 });
  console.log(JSON.stringify({ observed: true, operation }));
}
`;

const FAKE_RUNNER = `
import { writeFile } from "node:fs/promises";
import { join } from "node:path";

const [, , root, operation] = process.argv;
if (operation === "fail") {
  await writeFile(join(root, "runner.failed"), "observed\\n", { mode: 0o600 });
  console.error("synthetic runner stderr");
  process.exitCode = 7;
} else {
  await writeFile(join(root, "runner.succeeded"), "observed\\n", { mode: 0o600 });
  console.log(JSON.stringify({ observed: true, operation }));
}
`;

async function invoke(configPath) {
  try {
    const result = await execFileAsync(NODE, [ORCHESTRATOR, configPath], {
      encoding: "utf8",
      maxBuffer: 8 * 1024 * 1024,
      shell: false,
    });
    return { code: 0, stdout: result.stdout, stderr: result.stderr };
  } catch (error) {
    return {
      code: typeof error?.status === "number" ? error.status : 1,
      stdout: typeof error?.stdout === "string" ? error.stdout : "",
      stderr: typeof error?.stderr === "string" ? error.stderr : String(error?.message ?? error),
    };
  }
}

async function writeFixtureScripts(root) {
  const paths = {
    supervisor: join(root, "fake-supervisor.mjs"),
    control: join(root, "fake-control.mjs"),
    runner: join(root, "fake-runner.mjs"),
  };
  await Promise.all([
    writeFile(paths.supervisor, FAKE_SUPERVISOR, { mode: 0o700 }),
    writeFile(paths.control, FAKE_CONTROL, { mode: 0o700 }),
    writeFile(paths.runner, FAKE_RUNNER, { mode: 0o700 }),
  ]);
  return paths;
}

function command(name, commandPath, args) {
  return { name, command: commandPath, args };
}

function makeConfig(
  controlRoot,
  scripts,
  { controls, runnerOperation, supervisorOperation = "serve", cleanupOperations = ["shutdown"] },
) {
  const statePath = join(controlRoot, "supervisor-state.json");
  return {
    controlRoot,
    ports: [],
    timeoutMs: 5_000,
    supervisor: {
      ...command("supervisor", NODE, [scripts.supervisor, supervisorOperation, controlRoot]),
      readyStatePath: statePath,
      readyStatus: "serving",
    },
    controls: controls.map((operation, index) =>
      command(`control-${index + 1}-${operation}`, NODE, [scripts.control, controlRoot, operation]),
    ),
    runner: command("sdk-runner", NODE, [scripts.runner, controlRoot, runnerOperation]),
    cleanup: cleanupOperations.map((operation, index) =>
      command(`cleanup-${index + 1}-${operation}`, NODE, [scripts.control, controlRoot, operation]),
    ),
  };
}

async function runCase(baseRoot, scripts, name, options) {
  const caseRoot = await mkdtemp(join(baseRoot, `orchestrator-${name}-`));
  const controlRoot = join(caseRoot, "control");
  await mkdir(controlRoot, { mode: 0o700 });
  const configPath = join(caseRoot, "config.json");
  const config = makeConfig(controlRoot, scripts, options);
  if (options.existingCapture === true) {
    const captureRoot = join(controlRoot, "evidence", "orchestration");
    await mkdir(captureRoot, { recursive: true, mode: 0o700 });
    await writeFile(join(captureRoot, "stale.marker"), "preserve\\n", { mode: 0o600 });
  }
  if (options.existingReceipt === true) {
    config.receiptPath = join(controlRoot, "receipt.json");
    await writeFile(config.receiptPath, "receipt-sentinel\n", { mode: 0o600 });
  }
  await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 });
  try {
    const invocation = await invoke(configPath);
    if (invocation.stdout.trim() === "") {
      throw new Error(`orchestrator invocation emitted no JSON: ${invocation.stderr}`);
    }
    const result = JSON.parse(invocation.stdout.trim());
    return { caseRoot, controlRoot, receiptPath: config.receiptPath ?? null, invocation, result };
  } catch (error) {
    await rm(caseRoot, { force: true, recursive: true });
    throw error;
  }
}

function assertDirectChild(record, orchestratorPid) {
  assert.equal(record.parentPid, orchestratorPid);
  assert.equal(record.detached, false);
  assert.ok(record.exit);
  assert.ok(record.close);
  assert.equal(record.signalObserved, null);
  assert.ok(record.stderrPath);
  assert.ok(record.stdoutPath);
}

async function main() {
  const baseRoot = await realpath("/tmp");
  const fixtureRoot = await mkdtemp(join(baseRoot, "tl-orchestrator-fixtures-"));
  const scripts = await writeFixtureScripts(fixtureRoot);
  try {
    const staleReceipt = await runCase(fixtureRoot, scripts, "stale-receipt", {
      controls: ["start"],
      runnerOperation: "success",
      existingReceipt: true,
    });
    assert.equal(staleReceipt.invocation.code, 1);
    assert.equal(staleReceipt.result.status, "failed");
    assert.equal(staleReceipt.result.failure.phase, "orchestrator");
    assert.match(staleReceipt.result.failure.error.message, /receiptPath must be absent/u);
    assert.equal(staleReceipt.result.preflight, null);
    assert.equal(staleReceipt.result.captureRoot, null);
    assert.equal(staleReceipt.result.children.supervisor, null);
    assert.equal(staleReceipt.result.children.controls.length, 0);
    assert.equal(staleReceipt.result.children.runner, null);
    assert.equal(staleReceipt.result.children.cleanup.length, 0);
    assert.equal(staleReceipt.result.cleanup.commandsAttempted, 0);
    assert.equal(await readFile(staleReceipt.receiptPath, "utf8"), "receipt-sentinel\n");
    await assert.rejects(
      lstat(join(staleReceipt.controlRoot, "evidence")),
      (error) => error?.code === "ENOENT",
    );
    await rm(staleReceipt.caseRoot, { force: true, recursive: true });

    const staleCapture = await runCase(fixtureRoot, scripts, "stale-capture", {
      controls: ["start"],
      runnerOperation: "success",
      existingCapture: true,
    });
    assert.equal(staleCapture.invocation.code, 1);
    assert.equal(staleCapture.result.status, "failed");
    assert.equal(staleCapture.result.failure.phase, "orchestrator");
    assert.equal(staleCapture.result.preflight, null);
    assert.equal(staleCapture.result.captureRoot, null);
    assert.equal(staleCapture.result.children.supervisor, null);
    assert.equal(staleCapture.result.children.cleanup.length, 0);
    assert.equal(staleCapture.result.cleanup.commandsAttempted, 0);
    assert.equal(
      await readFile(join(staleCapture.controlRoot, "evidence/orchestration/stale.marker"), "utf8"),
      "preserve\\n",
    );
    await assert.rejects(
      lstat(join(staleCapture.controlRoot, "evidence/runner.stdout")),
      (error) => error?.code === "ENOENT",
    );
    await assert.rejects(
      lstat(join(staleCapture.controlRoot, "evidence/runner.stderr")),
      (error) => error?.code === "ENOENT",
    );
    await rm(staleCapture.caseRoot, { force: true, recursive: true });

    const success = await runCase(fixtureRoot, scripts, "success", {
      controls: ["start"],
      runnerOperation: "success",
    });
    if (success.invocation.code !== 0) {
      throw new Error(
        `success invocation failed: ${JSON.stringify({
          code: success.invocation.code,
          stdout: success.invocation.stdout,
          stderr: success.invocation.stderr,
        })}`,
      );
    }
    assert.equal(success.invocation.code, 0);
    assert.equal(success.result.status, "passed");
    assert.equal(success.result.trustMode, "sequential-macos-login-keychain");
    assertDirectChild(success.result.children.supervisor, success.result.orchestratorPid);
    assertDirectChild(success.result.children.controls[0], success.result.orchestratorPid);
    assertDirectChild(success.result.children.runner, success.result.orchestratorPid);
    assertDirectChild(success.result.children.cleanup[0], success.result.orchestratorPid);
    assert.equal(success.result.children.supervisor.exit.code, 0);
    assert.equal(success.result.children.runner.exit.code, 0);
    assert.equal(success.result.cleanup.supervisorClosed, true);
    assert.equal(await readFile(success.result.children.runner.stderrPath, "utf8"), "");
    assert.match(
      await readFile(join(success.controlRoot, "evidence/runner.stdout"), "utf8"),
      /observed/u,
    );
    assert.equal(
      await readFile(join(success.controlRoot, "runner.succeeded"), "utf8"),
      "observed\n",
    );
    await rm(success.caseRoot, { force: true, recursive: true });

    const supervisorFailure = await runCase(fixtureRoot, scripts, "supervisor-failure", {
      controls: ["start"],
      runnerOperation: "success",
      supervisorOperation: "fail",
    });
    assert.equal(supervisorFailure.invocation.code, 1);
    assert.equal(supervisorFailure.result.status, "failed");
    assert.equal(supervisorFailure.result.children.controls.length, 0);
    assert.equal(supervisorFailure.result.children.runner, null);
    assert.match(
      await readFile(supervisorFailure.result.children.supervisor.stderrPath, "utf8"),
      /synthetic supervisor stderr/u,
    );
    assert.equal(supervisorFailure.result.children.supervisor.exit.code, 12);
    await rm(supervisorFailure.caseRoot, { force: true, recursive: true });

    const runnerFailure = await runCase(fixtureRoot, scripts, "runner-failure", {
      controls: ["start"],
      runnerOperation: "fail",
    });
    assert.equal(runnerFailure.invocation.code, 1);
    assert.equal(runnerFailure.result.status, "failed");
    assert.equal(runnerFailure.result.children.runner.exit.code, 7);
    assert.match(
      await readFile(runnerFailure.result.children.runner.stderrPath, "utf8"),
      /synthetic runner stderr/u,
    );
    assert.match(
      await readFile(join(runnerFailure.controlRoot, "evidence/runner.stderr"), "utf8"),
      /synthetic runner stderr/u,
    );
    assert.equal(runnerFailure.result.children.cleanup[0].exit.code, 0);
    assert.equal(runnerFailure.result.cleanup.supervisorClosed, true);
    await rm(runnerFailure.caseRoot, { force: true, recursive: true });

    const controlFailure = await runCase(fixtureRoot, scripts, "control-failure", {
      controls: ["start", "fail"],
      runnerOperation: "success",
    });
    assert.equal(controlFailure.invocation.code, 1);
    assert.equal(controlFailure.result.status, "failed");
    assert.equal(controlFailure.result.children.controls.length, 2);
    assert.equal(controlFailure.result.children.controls[1].exit.code, 9);
    assert.equal(controlFailure.result.children.runner, null);
    assert.equal(controlFailure.result.children.cleanup[0].exit.code, 0);
    assert.equal(controlFailure.result.cleanup.supervisorClosed, true);
    assert.equal(
      await readFile(join(controlFailure.controlRoot, "runner.succeeded"), "utf8").catch(
        (error) => error.code,
      ),
      "ENOENT",
    );
    await rm(controlFailure.caseRoot, { force: true, recursive: true });

    const cleanupSequence = await runCase(fixtureRoot, scripts, "cleanup-sequence", {
      controls: ["start"],
      runnerOperation: "success",
      cleanupOperations: ["shutdown", "cleanup-failure", "after"],
    });
    assert.equal(cleanupSequence.invocation.code, 1);
    assert.equal(cleanupSequence.result.status, "failed");
    assert.equal(cleanupSequence.result.children.cleanup.length, 3);
    assert.deepEqual(
      cleanupSequence.result.children.cleanup.map((record) => record.args.at(-1)),
      ["shutdown", "cleanup-failure", "after"],
    );
    assert.equal(cleanupSequence.result.children.cleanup[0].exit.code, 0);
    assert.equal(cleanupSequence.result.children.cleanup[1].exit.code, 11);
    assert.equal(cleanupSequence.result.children.cleanup[2].exit.code, 0);
    assert.equal(cleanupSequence.result.cleanup.commandsConfigured, 3);
    assert.equal(cleanupSequence.result.cleanup.commandsAttempted, 3);
    assert.deepEqual(
      cleanupSequence.result.cleanup.attempts.map((attempt) => attempt.status),
      ["passed", "failed", "passed"],
    );
    assert.equal(
      cleanupSequence.result.cleanup.attempts[1].failure.phase,
      "cleanup:cleanup-2-cleanup-failure",
    );
    assert.equal(cleanupSequence.result.cleanup.supervisorClosed, true);
    assert.equal(
      await readFile(join(cleanupSequence.controlRoot, "cleanup-failure.attempted"), "utf8"),
      "observed\n",
    );
    assert.equal(
      await readFile(join(cleanupSequence.controlRoot, "control-after"), "utf8"),
      "observed\n",
    );
    await rm(cleanupSequence.caseRoot, { force: true, recursive: true });

    console.log(
      JSON.stringify({
        status: "passed",
        cases: [
          "stale-receipt-before-preflight-mutation",
          "stale-capture-before-preflight-mutation",
          "success",
          "supervisor-failure",
          "runner-failure",
          "control-failure",
          "cleanup-sequence-after-supervisor-close",
        ],
        browserStarted: false,
        hubContacted: false,
        keychainTouched: false,
        freshLiveFixture: false,
      }),
    );
  } finally {
    await rm(fixtureRoot, { force: true, recursive: true });
  }
}

main().catch((error) => {
  console.error(JSON.stringify({ status: "failed", error: String(error?.stack ?? error) }));
  process.exitCode = 1;
});
