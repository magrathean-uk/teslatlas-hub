import { execFileSync } from "node:child_process";

const npmExecutable = process.platform === "win32" ? "npm.cmd" : "npm";
const output = execFileSync(npmExecutable, ["pack", "--dry-run", "--json"], {
  encoding: "utf8",
});
const reports = JSON.parse(output);

if (!Array.isArray(reports) || reports.length !== 1 || !Array.isArray(reports[0]?.files)) {
  throw new Error("npm pack did not return one inspectable package report");
}

const files = new Set(reports[0].files.map((entry) => entry.path));
const required = [
  "LICENSE",
  "README.md",
  "docs/api.md",
  "docs/architecture.md",
  "docs/compatibility.md",
  "docs/protocol-dependency-gate.md",
  "dist/browser.d.ts",
  "dist/browser.js",
  "dist/index.d.ts",
  "dist/index.js",
  "dist/node.d.ts",
  "dist/node.js",
  "dist/generated/validators.d.ts",
  "dist/generated/validators.js",
  "package.json",
];
const forbiddenPrefixes = [
  ".github/",
  "docs/superpowers/",
  "protocol/",
  "scripts/",
  "src/",
  "tests/",
];
const missing = required.filter((path) => !files.has(path));
const forbidden = [...files].filter(
  (path) => path.endsWith(".map") || forbiddenPrefixes.some((prefix) => path.startsWith(prefix)),
);

if (missing.length > 0 || forbidden.length > 0) {
  throw new Error(
    `Invalid package contents. Missing: ${missing.join(", ") || "none"}. Forbidden: ${forbidden.join(", ") || "none"}.`,
  );
}

console.log(`Package contents verified: ${files.size} files`);
