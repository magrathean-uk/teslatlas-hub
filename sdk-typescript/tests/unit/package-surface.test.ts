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
    expect(Object.keys(manifest.exports).sort()).toEqual([
      ".",
      "./browser",
      "./node",
      "./package.json",
    ]);
    expect(manifest.files).toContain("docs/compatibility.md");
  });

  it("runtime-imports only the closed package entry points", async () => {
    const script = `
      const root = await import("@teslatlas/sdk");
      const browser = await import("@teslatlas/sdk/browser");
      const node = await import("@teslatlas/sdk/node");
      if (typeof root.TeslatlasError !== "function" || typeof root.asOpaqueCursor !== "function") throw new Error("root exports missing");
      if (typeof root.createClient !== "undefined") throw new Error("root factory leaked");
      for (const api of [root, browser, node]) {
        for (const name of ["FetchTransport", "parseSseStream", "subscribeToSse"]) {
          if (typeof api[name] !== "undefined") throw new Error("internal export leaked: " + name);
        }
      }
      if (typeof browser.createClient !== "function" || typeof node.createClient !== "function") throw new Error("runtime factory missing");
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

  it("packs public documentation and runtime internals without source maps or source inputs", async () => {
    const result = await execFileAsync(npmExecutable, ["pack", "--dry-run", "--json"], {
      cwd: repositoryRoot,
      encoding: "utf8",
    });
    const reports = JSON.parse(result.stdout) as Array<{ files?: Array<{ path?: string }> }>;
    const files = new Set(reports[0]?.files?.map((entry) => entry.path).filter(isString));

    expect(files.has("docs/compatibility.md")).toBe(true);
    expect(files.has("dist/generated/validators.js")).toBe(true);
    expect(files.has("dist/generated/validators.d.ts")).toBe(true);
    expect([...files].filter((path) => path.endsWith(".map"))).toEqual([]);
    expect(
      [...files].filter((path) =>
        [".github/", "docs/superpowers/", "protocol/", "scripts/", "src/", "tests/"].some(
          (prefix) => path.startsWith(prefix),
        ),
      ),
    ).toEqual([]);
  });
});

const npmExecutable = process.platform === "win32" ? "npm.cmd" : "npm";

function isString(value: string | undefined): value is string {
  return typeof value === "string";
}
