import {
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  realpath,
  rename,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { recoverProtocolSync, synchronizeProtocol } from "../../scripts/sync-protocol.mjs";

const generatedPaths = [
  "src/generated/protocol.ts",
  "src/generated/validators.ts",
  "src/generated/protocol-cases.ts",
  "src/generated/event-catalog.ts",
  "src/generated/hub-protocol.ts",
  "src/generated/hub-validators.js",
  "src/generated/hub-validators.d.ts",
] as const;
const commit = "a".repeat(40);

async function writeGeneratedOutputs(stageRoot: string) {
  await mkdir(join(stageRoot, "src/generated"), { recursive: true });
  for (const path of generatedPaths) await writeFile(join(stageRoot, path), `new:${path}\n`);
}

async function fixture() {
  const temporary = await mkdtemp(join(await realpath(tmpdir()), "teslatlas-sync-test-"));
  const root = join(temporary, "sdk");
  const checkout = join(temporary, "authority");
  await mkdir(join(root, "protocol/source/schemas"), { recursive: true });
  await mkdir(join(root, "src/generated"), { recursive: true });
  await mkdir(checkout);
  await writeFile(join(root, "protocol/source/schemas/old.schema.json"), '{"old":true}\n');
  await writeFile(
    join(root, "protocol/lock.json"),
    JSON.stringify({
      files: { "schemas/old.schema.json": "old-input-binding" },
      generated: { existing: "old-output-binding" },
      generator: { name: "synthetic-generator" },
      schemaVersion: 1,
      profiles: { richer: { inputSha256: "old-rich" }, currentHub: { status: "locked" } },
      source: { kind: "git-commit", repository: "https://example.invalid/protocol", commit },
    }),
  );
  for (const path of generatedPaths) await writeFile(join(root, path), `old:${path}\n`);
  const watched = [
    "protocol/source/schemas/old.schema.json",
    "protocol/lock.json",
    ...generatedPaths,
  ];
  const before = await Promise.all(watched.map((path) => readFile(join(root, path), "utf8")));
  const unchanged = async () => {
    expect(await Promise.all(watched.map((path) => readFile(join(root, path), "utf8")))).toEqual(
      before,
    );
    await expect(
      lstat(join(root, "protocol/source/schemas/new.schema.json")),
    ).rejects.toMatchObject({ code: "ENOENT" });
  };
  const options = {
    root,
    checkout,
    authorityCommit: commit,
    committedInputs: new Map([["schemas/new.schema.json", Buffer.from('{"new":true}\n')]]),
    generate: async (stageRoot: string) => {
      // Generation observes only staged inputs and the production tree is still coherent.
      await unchanged();
      expect(
        await readFile(join(stageRoot, "protocol/source/schemas/new.schema.json"), "utf8"),
      ).toBe('{"new":true}\n');
      expect(
        JSON.parse(await readFile(join(stageRoot, "protocol/lock.json"), "utf8")).generated,
      ).toEqual({});
      await writeGeneratedOutputs(stageRoot);
    },
  };
  return { temporary, root, options, unchanged };
}

describe("Protocol synchronization transaction", () => {
  it("can synchronize again after restoration cleanup is interrupted with its stage already removed", async () => {
    const current = await fixture();
    try {
      await expect(
        synchronizeProtocol({
          ...current.options,
          failpoint: async (stage, stageRoot) => {
            if (stage === "replaced:protocol/source") throw new Error("injected promotion failure");
            if (stage !== "recovery-journal-removed") return;
            await expect(
              lstat(join(current.root, "protocol/.sync-transaction.json")),
            ).rejects.toMatchObject({ code: "ENOENT" });
            await rm(stageRoot, { recursive: true, force: true });
            throw new Error("injected cleanup interruption");
          },
        }),
      ).rejects.toThrow("rollback failed");
      await current.unchanged();
      await recoverProtocolSync(current.root);
      await expect(synchronizeProtocol(current.options)).resolves.toBeDefined();
      expect(
        await readFile(join(current.root, "protocol/source/schemas/new.schema.json"), "utf8"),
      ).toBe('{"new":true}\n');
    } finally {
      await rm(current.temporary, { recursive: true, force: true });
    }
  });

  it("rejects an intermediate source symlink before creating any outside replacement directory", async () => {
    const current = await fixture();
    const retained = join(current.root, "src-retained");
    const outside = join(current.temporary, "outside");
    try {
      await rename(join(current.root, "src"), retained);
      await mkdir(outside);
      await symlink(outside, join(current.root, "src"));
      await expect(
        synchronizeProtocol({ ...current.options, generate: writeGeneratedOutputs }),
      ).rejects.toThrow("without symlink parents");
      expect(await readdir(outside)).toEqual([]);
      await expect(
        lstat(join(current.root, "protocol/.sync-transaction.json")),
      ).rejects.toMatchObject({ code: "ENOENT" });
      await rm(join(current.root, "src"));
      await rename(retained, join(current.root, "src"));
      await current.unchanged();
    } finally {
      await rm(current.temporary, { recursive: true, force: true });
    }
  });

  it.each([
    "inputs-staged",
    "initial-lock-staged",
    "generation-complete",
    "final-lock-staged",
    "replaced:protocol/source",
    "replaced:src/generated/protocol.ts",
    "replaced:protocol/lock.json",
  ])("preserves the previous input/output/lock state on failure at %s", async (failureStage) => {
    const current = await fixture();
    let stageRoot: string | undefined;
    try {
      await expect(
        synchronizeProtocol({
          ...current.options,
          failpoint: (stage, staged) => {
            stageRoot = staged;
            if (stage === failureStage) throw new Error(`injected:${stage}`);
          },
        }),
      ).rejects.toThrow(`injected:${failureStage}`);
      await current.unchanged();
      await expect(
        lstat(join(current.root, "protocol/.sync-transaction.json")),
      ).rejects.toMatchObject({ code: "ENOENT" });
      expect(stageRoot).toBeDefined();
      if (stageRoot === undefined) throw new Error("staging directory was not observed");
      await expect(lstat(stageRoot)).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await rm(current.temporary, { recursive: true, force: true });
    }
  });

  it("preserves all current files when generation fails after its first output", async () => {
    const current = await fixture();
    try {
      await expect(
        synchronizeProtocol({
          ...current.options,
          generate: async (stageRoot) => {
            await mkdir(join(stageRoot, "src/generated"), { recursive: true });
            await writeFile(join(stageRoot, generatedPaths[0]), "partially generated");
            throw new Error("injected generator failure");
          },
        }),
      ).rejects.toThrow("injected generator failure");
      await current.unchanged();
    } finally {
      await rm(current.temporary, { recursive: true, force: true });
    }
  });

  it("replaces source, every generated output and a complete final lock after staged generation", async () => {
    const current = await fixture();
    try {
      const result = await synchronizeProtocol(current.options);
      expect(
        await readFile(join(current.root, "protocol/source/schemas/new.schema.json"), "utf8"),
      ).toBe('{"new":true}\n');
      await expect(
        lstat(join(current.root, "protocol/source/schemas/old.schema.json")),
      ).rejects.toMatchObject({ code: "ENOENT" });
      for (const path of generatedPaths)
        expect(await readFile(join(current.root, path), "utf8")).toBe(`new:${path}\n`);
      const lock = JSON.parse(await readFile(join(current.root, "protocol/lock.json"), "utf8"));
      expect(lock).toEqual(result);
      expect(Object.keys(lock.generated).sort()).toEqual([...generatedPaths].sort());
      expect(Object.values(lock.generated)).toEqual(
        expect.arrayContaining([expect.stringMatching(/^[0-9a-f]{64}$/u)]),
      );
      await expect(
        lstat(join(current.root, "protocol/.sync-transaction.json")),
      ).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await rm(current.temporary, { recursive: true, force: true });
    }
  });

  it("retains backups when rollback fails and recovers after the disposable obstruction is repaired", async () => {
    const current = await fixture();
    let stageRoot = "";
    try {
      await expect(
        synchronizeProtocol({
          ...current.options,
          failpoint: async (stage, staged) => {
            if (stage !== "replaced:protocol/source") return;
            stageRoot = staged;
            const backup = join(staged, ".backup/protocol/source");
            await rename(backup, `${backup}-retained`);
            await symlink(`${backup}-retained`, backup);
            throw new Error("injected replacement failure");
          },
        }),
      ).rejects.toThrow("rollback failed");
      expect(
        await readFile(
          join(stageRoot, ".backup/protocol/source-retained/schemas/old.schema.json"),
          "utf8",
        ),
      ).toBe('{"old":true}\n');
      await expect(recoverProtocolSync(current.root)).rejects.toThrow("owns the recovery journal");
      const backup = join(stageRoot, ".backup/protocol/source");
      await rm(backup);
      await rename(`${backup}-retained`, backup);
      await recoverProtocolSync(current.root, process.pid);
      await current.unchanged();
      await expect(lstat(stageRoot)).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await rm(current.temporary, { recursive: true, force: true });
      if (stageRoot) await rm(stageRoot, { recursive: true, force: true });
    }
  });

  it("rejects a symlinked generated output before replacing any current file", async () => {
    const current = await fixture();
    try {
      await expect(
        synchronizeProtocol({
          ...current.options,
          generate: async (stageRoot) => {
            await current.options.generate(stageRoot);
            const output = join(stageRoot, generatedPaths[0]);
            await rm(output);
            await symlink(join(current.root, generatedPaths[0]), output);
          },
        }),
      ).rejects.toThrow("regular file");
      await current.unchanged();
    } finally {
      await rm(current.temporary, { recursive: true, force: true });
    }
  });

  it("rejects candidate input symlinks without touching the previous synchronized state", async () => {
    const current = await fixture();
    try {
      for (const directory of [
        "openapi",
        "events",
        "schemas",
        "examples",
        "fixtures",
        "compatibility",
        "conformance/cases",
      ]) {
        await mkdir(join(current.options.checkout, directory), { recursive: true });
      }
      await symlink(
        join(current.root, "protocol/source/schemas/old.schema.json"),
        join(current.options.checkout, "schemas/linked.schema.json"),
      );
      await expect(
        synchronizeProtocol({ ...current.options, candidateMode: true }),
      ).rejects.toThrow("symlinks");
      await current.unchanged();
    } finally {
      await rm(current.temporary, { recursive: true, force: true });
    }
  });
});
