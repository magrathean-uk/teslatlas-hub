import assert from "node:assert/strict";
import test from "node:test";

import { normalizeNpmPackReport } from "./npm-pack-report.mjs";

const report = {
  name: "@tesatlas/sdk",
  version: "2026.36.2",
  filename: "tesatlas-sdk-2026.36.2.tgz",
  entryCount: 88,
  files: [],
};

test("normalizes npm 12's package-keyed JSON report", () => {
  assert.deepEqual(normalizeNpmPackReport({ "@tesatlas/sdk": report }, "@tesatlas/sdk"), report);
});

test("keeps compatibility with npm's historical one-element array", () => {
  assert.deepEqual(normalizeNpmPackReport([report], "@tesatlas/sdk"), report);
});

test("rejects mismatched or ambiguous package reports", () => {
  assert.throws(
    () => normalizeNpmPackReport({ "@other/package": report }, "@tesatlas/sdk"),
    /shape is invalid/u,
  );
  assert.throws(
    () => normalizeNpmPackReport([report, report], "@tesatlas/sdk"),
    /exactly one report/u,
  );
  assert.throws(
    () =>
      normalizeNpmPackReport({ "@tesatlas/sdk": { ...report, name: "wrong" } }, "@tesatlas/sdk"),
    /package identity is invalid/u,
  );
});
