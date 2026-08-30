import { execFile, spawn, type ChildProcess } from "node:child_process";
import { once } from "node:events";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { chromium } from "playwright";
import { describe, expect, it } from "vitest";

const execFileAsync = promisify(execFile);
const repositoryRoot = fileURLToPath(new URL("../..", import.meta.url));

describe("runnable examples", () => {
  it("runs the deterministic Node.js transport example", async () => {
    const result = await execFileAsync(process.execPath, ["examples/node.mjs"], {
      cwd: repositoryRoot,
      encoding: "utf8",
    });

    expect(result.stderr).toBe("");
    expect(result.stdout).toBe('Teslatlas SDK transport example: 304 "fixture-2"\n');
  });

  it("loads the built browser example in Chromium", async () => {
    const server = spawn(process.execPath, ["examples/browser/serve.mjs"], {
      cwd: repositoryRoot,
      env: { ...process.env, TESLATLAS_EXAMPLE_PORT: "0" },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;

    try {
      const url = await waitForExampleUrl(server);
      browser = await chromium.launch({ headless: true });
      const page = await browser.newPage();
      const pageErrors: string[] = [];
      page.on("pageerror", (error) => pageErrors.push(error.message));

      const response = await page.goto(url, { waitUntil: "domcontentloaded" });

      expect(response?.status()).toBe(200);
      await expect
        .poll(() => page.locator("#output").textContent())
        .toBe('Teslatlas SDK browser transport: 304 "fixture-2"');
      expect(pageErrors).toEqual([]);
    } finally {
      await browser?.close();
      await stopProcess(server);
    }
  }, 30_000);
});

function waitForExampleUrl(child: ChildProcess): Promise<string> {
  return new Promise((resolve, reject) => {
    let stdout = "";
    let stderr = "";
    const timeout = setTimeout(
      () => finish(new Error("Browser example server did not start")),
      10_000,
    );

    const finish = (error: Error | undefined, url?: string) => {
      clearTimeout(timeout);
      child.stdout?.off("data", onStdout);
      child.stderr?.off("data", onStderr);
      child.off("exit", onExit);
      if (error !== undefined) {
        reject(error);
      } else if (url !== undefined) {
        resolve(url);
      }
    };
    const onStdout = (chunk: Buffer | string) => {
      stdout += chunk.toString();
      const match = stdout.match(/http:\/\/127\.0\.0\.1:\d+/u);
      if (match?.[0] !== undefined) {
        finish(undefined, match[0]);
      }
    };
    const onStderr = (chunk: Buffer | string) => {
      stderr += chunk.toString();
    };
    const onExit = (code: number | null) => {
      finish(new Error(`Browser example server exited with ${code}: ${stderr}`));
    };

    child.stdout?.on("data", onStdout);
    child.stderr?.on("data", onStderr);
    child.once("exit", onExit);
  });
}

async function stopProcess(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return;
  }
  const exited = once(child, "exit");
  child.kill("SIGTERM");
  await exited;
}
