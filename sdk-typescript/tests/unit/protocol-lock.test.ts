import { execFile } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";

const execFileAsync = promisify(execFile);
const root = fileURLToPath(new URL("../..", import.meta.url));

describe("protocol lock", () => {
  it("pins the validated authority revision and all three profiles", async () => {
    const lock = JSON.parse(
      await readFile(new URL("../../protocol/lock.json", import.meta.url), "utf8"),
    );
    expect(lock).toMatchObject({
      commit: "79ced4c7fdc79520ad31d72a0280bf5f3f19f407",
      currentProfile: "1.2.0",
      supportedProfiles: ["1.0.0", "1.1.0", "1.2.0"],
      generator: { package: "openapi-typescript", version: "7.13.0" },
    });
    expect(Object.keys(lock.files).length).toBeGreaterThan(40);
  });

  it("passes the offline byte and generation check", async () => {
    const result = await execFileAsync(process.execPath, ["scripts/check-protocol.mjs"], {
      cwd: root,
      encoding: "utf8",
    });
    expect(result.stderr).toBe("");
    expect(result.stdout).toContain("Protocol lock verified");
  });

  it("rejects a tampered committed generated file", async () => {
    const generated = new URL("../../src/generated/protocol.ts", import.meta.url);
    const original = await readFile(generated);
    try {
      await writeFile(generated, Buffer.concat([original, Buffer.from("\n// tampered\n")]));
      await expect(
        execFileAsync(process.execPath, ["scripts/check-protocol.mjs"], {
          cwd: root,
          encoding: "utf8",
        }),
      ).rejects.toThrow("Protocol generated hash mismatch: src/generated/protocol.ts");
    } finally {
      await writeFile(generated, original);
    }
  });
});
