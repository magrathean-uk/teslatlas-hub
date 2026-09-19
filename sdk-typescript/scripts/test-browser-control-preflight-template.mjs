import assert from "node:assert/strict";
import {
  chmod,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  stat,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { prepareBrowserControlFiles } from "./browser-control-preflight-template.mjs";

const PORTS = [4176, 9236, 9237];
const EVIDENCE_DIR = "evidence";
const RUNNER_OUTPUTS = ["runner.stdout", "runner.stderr"];
const STALE_ARTIFACTS = [
  "control.sock",
  "supervisor-state.json",
  "evidence/browser-control-events.jsonl",
  "untrusted-profile",
  "trusted-profile",
  "evidence/imported-hub-ca.pem",
  "evidence/browser-trust-witness.json",
  "evidence/browser-acceptance-receipt.json",
  "evidence/runner.stdout",
  "evidence/runner.stderr",
];

async function mode(path) {
  return (await stat(path)).mode & 0o777;
}

async function createFixture() {
  const root = await mkdtemp(join(tmpdir(), "teslatlas-browser-preflight-template-"));
  await mkdir(join(root, EVIDENCE_DIR));
  return root;
}

async function createStaleArtifactFixture(relativePath) {
  const root = await createFixture();
  await chmod(root, 0o755);
  await chmod(join(root, EVIDENCE_DIR), 0o755);
  const path = join(root, relativePath);
  if (relativePath.endsWith("profile")) {
    await mkdir(path, { mode: 0o755 });
    await chmod(path, 0o755);
  } else {
    await writeFile(path, "sentinel", { mode: 0o644 });
    await chmod(path, 0o644);
  }
  return { root, path };
}

async function createRootSymlinkFixture() {
  const parent = await mkdtemp(join(tmpdir(), "teslatlas-browser-preflight-symlink-"));
  const target = join(parent, "target-root");
  const rootLink = join(parent, "control-root");
  await mkdir(target);
  await mkdir(join(target, EVIDENCE_DIR));
  await chmod(target, 0o755);
  await chmod(join(target, EVIDENCE_DIR), 0o755);
  await writeFile(join(target, "sentinel"), "target-sentinel", { mode: 0o644 });
  await symlink(target, rootLink, "dir");
  return { parent, rootLink, target };
}

async function main() {
  const freshRoot = await createFixture();
  try {
    const first = await prepareBrowserControlFiles({ controlRoot: freshRoot, ports: PORTS });
    assert.equal(first.status, "passed");
    assert.equal(first.browserStarted, false);
    assert.equal(first.hubContacted, false);
    assert.equal(first.keychainTouched, false);
    assert.deepEqual(first.runnerOutputFiles, RUNNER_OUTPUTS);
    for (const output of RUNNER_OUTPUTS) {
      assert.equal(await mode(join(freshRoot, EVIDENCE_DIR, output)), 0o600);
    }

    const stdoutPath = join(freshRoot, EVIDENCE_DIR, "runner.stdout");
    const stderrPath = join(freshRoot, EVIDENCE_DIR, "runner.stderr");
    await writeFile(stdoutPath, "stdout-sentinel", { flag: "w" });
    await writeFile(stderrPath, "stderr-sentinel", { flag: "w" });
    await assert.rejects(
      () => prepareBrowserControlFiles({ controlRoot: freshRoot, ports: PORTS }),
      /runtime artifact must be absent/,
    );
    assert.equal(await readFile(stdoutPath, "utf8"), "stdout-sentinel");
    assert.equal(await readFile(stderrPath, "utf8"), "stderr-sentinel");
  } finally {
    await rm(freshRoot, { recursive: true, force: true });
  }

  for (const artifact of STALE_ARTIFACTS) {
    const fixture = await createStaleArtifactFixture(artifact);
    try {
      await assert.rejects(
        () => prepareBrowserControlFiles({ controlRoot: fixture.root, ports: PORTS }),
        /runtime artifact must be absent/,
      );
      assert.equal(await mode(fixture.root), 0o755, `${artifact} changed root before rejection`);
      assert.equal(
        await mode(join(fixture.root, EVIDENCE_DIR)),
        0o755,
        `${artifact} changed evidence mode before rejection`,
      );
      if (artifact.endsWith("profile")) {
        assert.equal(
          await mode(fixture.path),
          0o755,
          `${artifact} changed profile before rejection`,
        );
      } else {
        assert.equal(await readFile(fixture.path, "utf8"), "sentinel");
      }
      for (const output of RUNNER_OUTPUTS) {
        const outputPath = join(fixture.root, EVIDENCE_DIR, output);
        if (outputPath === fixture.path) continue;
        await assert.rejects(lstat(outputPath), { code: "ENOENT" });
      }
    } finally {
      await rm(fixture.root, { recursive: true, force: true });
    }
  }

  const symlinkFixture = await createRootSymlinkFixture();
  try {
    await assert.rejects(
      () => prepareBrowserControlFiles({ controlRoot: symlinkFixture.rootLink, ports: PORTS }),
      /controlRoot must be a real directory/,
    );
    assert.equal(await mode(symlinkFixture.target), 0o755);
    assert.equal(await mode(join(symlinkFixture.target, EVIDENCE_DIR)), 0o755);
    assert.equal(
      await readFile(join(symlinkFixture.target, "sentinel"), "utf8"),
      "target-sentinel",
    );
    for (const output of RUNNER_OUTPUTS) {
      await assert.rejects(lstat(join(symlinkFixture.target, EVIDENCE_DIR, output)), {
        code: "ENOENT",
      });
    }
  } finally {
    await rm(symlinkFixture.parent, { recursive: true, force: true });
  }

  console.log(
    JSON.stringify({
      status: "passed",
      browserStarted: false,
      hubContacted: false,
      keychainTouched: false,
      firstInvocationCreated: RUNNER_OUTPUTS,
      secondInvocation: "rejected_without_truncation",
      staleArtifactsRejectedBeforeMutation: STALE_ARTIFACTS,
      rootSymlinkRejectedBeforeMutation: true,
    }),
  );
}

await main();
