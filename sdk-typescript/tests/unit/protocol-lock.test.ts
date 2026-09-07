import { execFile } from "node:child_process";
import { access, cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";
import {
  candidateSourceIdentity,
  filesForProfile,
  sha256DigestMap,
} from "../../scripts/protocol-files.mjs";

const execFileAsync = promisify(execFile);
const root = fileURLToPath(new URL("../..", import.meta.url));

async function makeSdkFixture(directory: string): Promise<string> {
  const sdk = join(directory, "sdk");
  await mkdir(join(sdk, "src"), { recursive: true });
  await Promise.all([
    cp(join(root, "protocol"), join(sdk, "protocol"), { recursive: true }),
    cp(join(root, "scripts"), join(sdk, "scripts"), { recursive: true }),
    cp(join(root, "src/generated"), join(sdk, "src/generated"), { recursive: true }),
    cp(join(root, "package.json"), join(sdk, "package.json")),
    cp(join(root, "package-lock.json"), join(sdk, "package-lock.json")),
  ]);
  await symlink(join(root, "node_modules"), join(sdk, "node_modules"), "dir");
  return sdk;
}

async function makeProtocolAuthority(directory: string): Promise<{ root: string; commit: string }> {
  const authority = join(directory, "authority");
  await cp(join(root, "protocol/source"), authority, { recursive: true });
  await execFileAsync("git", ["init", "-q"], { cwd: authority });
  await execFileAsync("git", ["config", "user.name", "Protocol Test"], { cwd: authority });
  await execFileAsync("git", ["config", "user.email", "protocol-test@example.invalid"], {
    cwd: authority,
  });
  await execFileAsync("git", ["add", "."], { cwd: authority });
  await execFileAsync("git", ["commit", "-q", "-m", "protocol fixture"], { cwd: authority });
  const { stdout } = await execFileAsync("git", ["rev-parse", "HEAD"], {
    cwd: authority,
    encoding: "utf8",
  });
  return { root: authority, commit: stdout.trim() };
}

function runFixtureScript(sdk: string, script: string, args: string[] = []) {
  return execFileAsync(process.execPath, [join(sdk, "scripts", script), ...args], {
    cwd: sdk,
    encoding: "utf8",
  });
}

describe("protocol lock", () => {
  it("binds a local candidate source identity to exported content", () => {
    expect(
      candidateSourceIdentity(
        "https://github.com/magrathean-uk/teslatlas-protocol.git",
        "79ced4c7fdc79520ad31d72a0280bf5f3f19f407",
        { "profiles/hub-http-v1/1.0.0/profile.json": "a".repeat(64) },
      ),
    ).toEqual({
      kind: "local-content",
      repository: "https://github.com/magrathean-uk/teslatlas-protocol.git",
      baseCommit: "79ced4c7fdc79520ad31d72a0280bf5f3f19f407",
      contentSha256: "78dbf54df8c5cb8df9087edba98accab28e32748aa430ad01a05d60ac9a6590f",
    });
  });

  it("validates the selected source identity and profile relationships", async () => {
    const lock = JSON.parse(
      await readFile(new URL("../../protocol/lock.json", import.meta.url), "utf8"),
    );
    expect(lock).toMatchObject({
      schemaVersion: 2,
      profiles: {
        richer: {
          id: "teslatlas-public",
          revision: "1.2.0",
          supportedRevisions: ["1.0.0", "1.1.0", "1.2.0"],
        },
        currentHub: {
          bundleSha256: "b3914d35d28374f6423af789e9ed6a4a4c82196a068c041946e24d609db0b05b",
          id: "hub-http-v1",
          revision: "1.0.0",
          status: "candidate",
          generatedOutputSha256: expect.stringMatching(/^[0-9a-f]{64}$/u),
        },
      },
      generator: { package: "openapi-typescript", version: "7.13.0" },
    });
    expect(Object.keys(lock.files).length).toBeGreaterThan(40);
    expect(lock.profiles.richer.inputSha256).toBe(
      sha256DigestMap(filesForProfile(lock.files, "richer")),
    );
    expect(lock.profiles.richer.generatedOutputSha256).toBe(
      sha256DigestMap(
        Object.fromEntries(
          [
            "src/generated/protocol.ts",
            "src/generated/validators.ts",
            "src/generated/protocol-cases.ts",
          ].map((path) => [path, lock.generated[path]]),
        ),
      ),
    );
    const currentHubFiles = filesForProfile(lock.files, "currentHub");
    expect(lock.profiles.currentHub.inputSha256).toBe(
      Object.keys(currentHubFiles).length === 0 ? null : sha256DigestMap(currentHubFiles),
    );
    expect(lock.profiles.currentHub.generatedOutputSha256).toBe(
      sha256DigestMap(
        Object.fromEntries(
          [
            "src/generated/hub-protocol.ts",
            "src/generated/hub-validators.js",
            "src/generated/hub-validators.d.ts",
          ].map((path) => [path, lock.generated[path]]),
        ),
      ),
    );
    if (lock.source.kind === "git-commit") {
      expect(lock.source.commit).toMatch(/^[0-9a-f]{40}$/);
    } else {
      expect(lock.source).toMatchObject({ kind: "local-content" });
      expect(lock.source.baseCommit).toMatch(/^[0-9a-f]{40}$/);
      expect(lock.source.contentSha256).toBe(sha256DigestMap(lock.files));
    }
  });

  it("commit sync reads the Git object and ignores dirty matching inputs", async () => {
    const directory = await mkdtemp(join(tmpdir(), "teslatlas-sdk-commit-sync-"));
    try {
      const sdk = await makeSdkFixture(directory);
      const authority = await makeProtocolAuthority(directory);
      const lockPath = join(sdk, "protocol/lock.json");
      const lock = JSON.parse(await readFile(lockPath, "utf8"));
      lock.source = {
        kind: "git-commit",
        repository: lock.source.repository,
        commit: authority.commit,
      };
      await writeFile(lockPath, `${JSON.stringify(lock, null, 2)}\n`);

      const tracked = join(authority.root, "openapi/teslatlas-v1.openapi.json");
      const committedBytes = await readFile(tracked);
      await writeFile(tracked, Buffer.concat([committedBytes, Buffer.from("\n")]));
      const untracked = join(authority.root, "profiles/hub-http-v1/1.0.0/review-untracked.json");
      await mkdir(dirname(untracked), { recursive: true });
      await writeFile(untracked, '{"candidate":true}\n');

      await runFixtureScript(sdk, "sync-protocol.mjs", [authority.root]);

      expect(
        await readFile(join(sdk, "protocol/source/openapi/teslatlas-v1.openapi.json")),
      ).toEqual(committedBytes);
      await expect(
        access(join(sdk, "protocol/source/profiles/hub-http-v1/1.0.0/review-untracked.json")),
      ).rejects.toThrow();
      const synced = JSON.parse(await readFile(lockPath, "utf8"));
      expect(synced.source).toMatchObject({ kind: "git-commit", commit: authority.commit });
      expect(synced.files).not.toHaveProperty("profiles/hub-http-v1/1.0.0/review-untracked.json");
      await expect(runFixtureScript(sdk, "check-protocol.mjs")).resolves.toMatchObject({
        stdout: expect.stringContaining("Protocol lock verified"),
      });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }, 30_000);

  it("candidate sync and check are stable in an isolated SDK fixture", async () => {
    const directory = await mkdtemp(join(tmpdir(), "teslatlas-sdk-candidate-sync-"));
    try {
      const sdk = await makeSdkFixture(directory);
      const authority = await makeProtocolAuthority(directory);
      const profile = join(authority.root, "profiles/hub-http-v1/1.0.0/review-candidate.json");
      await mkdir(dirname(profile), { recursive: true });
      await writeFile(profile, '{"profile":"hub-http-v1@1.0.0"}\n');

      await runFixtureScript(sdk, "sync-protocol.mjs", ["--candidate", authority.root]);
      const lockPath = join(sdk, "protocol/lock.json");
      const firstLock = await readFile(lockPath, "utf8");
      const firstGenerated = await Promise.all(
        ["protocol.ts", "validators.ts", "protocol-cases.ts"].map((name) =>
          readFile(join(sdk, "src/generated", name)),
        ),
      );
      const lock = JSON.parse(firstLock);
      expect(lock.source).toMatchObject({ kind: "local-content", baseCommit: authority.commit });
      expect(lock.source.contentSha256).toBe(sha256DigestMap(lock.files));
      expect(lock.profiles.currentHub.inputSha256).toBe(
        sha256DigestMap(filesForProfile(lock.files, "currentHub")),
      );
      await expect(runFixtureScript(sdk, "check-protocol.mjs")).resolves.toMatchObject({
        stdout: expect.stringContaining("Protocol lock verified"),
      });

      await runFixtureScript(sdk, "sync-protocol.mjs", ["--candidate", authority.root]);

      expect(await readFile(lockPath, "utf8")).toBe(firstLock);
      const secondGenerated = await Promise.all(
        ["protocol.ts", "validators.ts", "protocol-cases.ts"].map((name) =>
          readFile(join(sdk, "src/generated", name)),
        ),
      );
      expect(secondGenerated).toEqual(firstGenerated);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }, 30_000);

  it("rejects a mutated input digest using an isolated SDK fixture", async () => {
    const directory = await mkdtemp(join(tmpdir(), "teslatlas-sdk-lock-mutation-"));
    try {
      const sdk = await makeSdkFixture(directory);
      const lockPath = join(sdk, "protocol/lock.json");
      const lock = JSON.parse(await readFile(lockPath, "utf8"));
      lock.profiles.richer.inputSha256 = "0".repeat(64);
      await writeFile(lockPath, `${JSON.stringify(lock, null, 2)}\n`);

      await expect(runFixtureScript(sdk, "check-protocol.mjs")).rejects.toThrow(
        "Richer protocol input digest does not match the pinned file map",
      );
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("passes the offline byte and generation check", async () => {
    const result = await execFileAsync(process.execPath, ["scripts/check-protocol.mjs"], {
      cwd: root,
      encoding: "utf8",
    });
    expect(result.stderr).toBe("");
    expect(result.stdout).toContain("Protocol lock verified");
  });

  it("rejects a mutated generated file using an isolated SDK fixture", async () => {
    const directory = await mkdtemp(join(tmpdir(), "teslatlas-sdk-generated-mutation-"));
    try {
      const sdk = await makeSdkFixture(directory);
      const generated = join(sdk, "src/generated/protocol.ts");
      await writeFile(
        generated,
        Buffer.concat([await readFile(generated), Buffer.from("\n// tampered\n")]),
      );

      await expect(runFixtureScript(sdk, "check-protocol.mjs")).rejects.toThrow(
        "Protocol generated hash mismatch: src/generated/protocol.ts",
      );
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });
});
