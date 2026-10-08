import { mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
  browserTrustHookPayload,
  cleanupResources,
  runTrustHook,
} from "../../scripts/browser-hub-runner.mjs";

describe("browser runner trust-hook contract", () => {
  it("executes a disposable JSON hook with literal argv and rejects malformed output", async () => {
    const root = await mkdtemp(join(await realpath(tmpdir()), "teslatlas-hook-test-"));
    try {
      const hook = join(root, "hook.mjs");
      await writeFile(hook, "process.stdout.write(JSON.stringify(JSON.parse(process.argv[2])));\n");
      const payload = { operation: "synthetic", literal: "$(echo unsafe); `echo unsafe`" };
      expect(await runTrustHook([process.execPath, hook], payload, { timeoutMs: 2_000 })).toEqual(
        payload,
      );
      await writeFile(hook, 'process.stdout.write("not JSON");\n');
      await expect(
        runTrustHook([process.execPath, hook], {}, { timeoutMs: 2_000 }),
      ).rejects.toThrow("invalid JSON");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  it("terminates an owned hook that ignores graceful termination at its deadline", async () => {
    const root = await mkdtemp(join(await realpath(tmpdir()), "teslatlas-hanging-hook-test-"));
    try {
      const hook = join(root, "hook.mjs");
      const pidPath = join(root, "owned-pid");
      await writeFile(
        hook,
        'import { writeFileSync } from "node:fs";\n' +
          'process.on("SIGTERM", () => undefined);\n' +
          "writeFileSync(process.argv[2], String(process.pid));\n" +
          "setInterval(() => undefined, 1000);\n",
      );
      await expect(
        runTrustHook([process.execPath, hook, pidPath], {}, { timeoutMs: 500 }),
      ).rejects.toMatchObject({
        killed: true,
        signal: "SIGKILL",
      });
      const pid = Number(await readFile(pidPath, "utf8"));
      expect(() => process.kill(pid, 0)).toThrow();
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  it("bounds each pending browser/server close and retains explicit cleanup errors", async () => {
    const events: string[] = [];
    const resources = {
      browsers: new Set([
        {
          close: () => {
            events.push("browser:pending");
            return new Promise<void>(() => undefined);
          },
        },
        {
          close: async () => {
            events.push("browser:closed");
          },
        },
      ]),
      server: {
        listening: true,
        close: (_callback: (error?: Error) => void) => {
          events.push("server:pending");
        },
        closeAllConnections: () => {
          events.push("server:connections-closed");
        },
      },
    };
    const errors = await cleanupResources(resources, 5);
    expect(errors).toHaveLength(2);
    expect(events).toEqual([
      "browser:pending",
      "browser:closed",
      "server:pending",
      "server:connections-closed",
    ]);
    expect(resources.browsers.size).toBe(1);
  });

  it("uses the reviewed hook keys for import, browser cleanup, removal, and removal verification", () => {
    const binding = {
      endpoint: "https://localhost:18537",
      certificatePath: "/private/hub/server.pem",
      certificateSha256: "a".repeat(64),
    };
    const trustedCdpUrl = "http://127.0.0.1:9248";
    const expectedCertificateExportPath = "/private/control/imported-hub-ca.pem";
    const fingerprint = "b".repeat(64);

    expect(
      browserTrustHookPayload("import", {
        ...binding,
        trustedCdpUrl,
        expectedCertificateExportPath,
      }),
    ).toEqual({
      operation: "import",
      ...binding,
      trustedCdpUrl,
      expectedCertificateExportPath,
    });

    expect(
      browserTrustHookPayload("trusted-browser-cleanup", {
        ...binding,
        fingerprint,
        trustedCdpUrl,
        expectedCertificateExportPath,
        witnessPath: "/private/control/browser-trust-witness.json",
      }),
    ).toEqual({
      operation: "trusted-browser-cleanup",
      ...binding,
      fingerprint,
      trustedCdpUrl,
      expectedCertificateExportPath,
      witnessPath: "/private/control/browser-trust-witness.json",
    });

    for (const operation of ["remove", "verify-removal"] as const) {
      expect(browserTrustHookPayload(operation, { ...binding, fingerprint })).toEqual({
        operation,
        ...binding,
        fingerprint,
      });
    }
  });
});
