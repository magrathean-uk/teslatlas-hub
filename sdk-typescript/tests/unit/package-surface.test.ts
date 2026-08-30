import { execFile } from "node:child_process";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import manifest from "../../package.json" with { type: "json" };
import { describe, expect, it } from "vitest";

const execFileAsync = promisify(execFile);
const repositoryRoot = fileURLToPath(new URL("../..", import.meta.url));

describe("package surface", () => {
  it("keeps the development package non-publishable", () => {
    expect(manifest.private).toBe(true);
  });

  it("uses one implementation for ESM imports", () => {
    expect(manifest.exports["."].import).toBe(manifest.exports["."].default);
    expect(manifest.exports["./browser"].import).toBe(manifest.exports["./browser"].default);
    expect(manifest.exports["./node"].import).toBe(manifest.exports["./node"].default);
  });

  it("runtime-imports every built package entry point", async () => {
    const script = `
      const root = await import("@teslatlas/sdk");
      const browser = await import("@teslatlas/sdk/browser");
      const node = await import("@teslatlas/sdk/node");
      if (typeof root.FetchTransport !== "function" || typeof root.parseProtocolVersion !== "function") throw new Error("root exports missing");
      if (typeof browser.createBrowserTransport !== "function") throw new Error("browser export missing");
      if (typeof node.createNodeTransport !== "function") throw new Error("node export missing");
      const paths = [
        import.meta.resolve("@teslatlas/sdk"),
        import.meta.resolve("@teslatlas/sdk/browser"),
        import.meta.resolve("@teslatlas/sdk/node"),
      ];
      process.stdout.write(JSON.stringify(paths));
    `;

    const result = await execFileAsync(
      process.execPath,
      ["--input-type=module", "--eval", script],
      {
        cwd: repositoryRoot,
        encoding: "utf8",
      },
    );

    expect(result.stderr).toBe("");
    const paths = JSON.parse(result.stdout) as string[];
    expect(paths.map((path) => new URL(path).pathname)).toEqual([
      expect.stringMatching(/\/dist\/index\.js$/u),
      expect.stringMatching(/\/dist\/browser\.js$/u),
      expect.stringMatching(/\/dist\/node\.js$/u),
    ]);
  });
});
