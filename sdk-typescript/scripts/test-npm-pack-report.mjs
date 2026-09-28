import assert from "node:assert/strict";
import test from "node:test";

import { normalizeNpmPackReport } from "./npm-pack-report.mjs";

const report = {
  name: "@teslatlas/sdk",
  version: "2026.36.2",
  filename: "teslatlas-sdk-2026.36.2.tgz",
  entryCount: 88,
  files: [],
};

test("normalizes npm 12's package-keyed JSON report", () => {
  assert.deepEqual(normalizeNpmPackReport({ "@teslatlas/sdk": report }, "@teslatlas/sdk"), report);
});

test("keeps compatibility with npm's historical one-element array", () => {
  assert.deepEqual(normalizeNpmPackReport([report], "@teslatlas/sdk"), report);
});

test("rejects mismatched or ambiguous package reports", () => {
  assert.throws(
    () => normalizeNpmPackReport({ "@other/package": report }, "@teslatlas/sdk"),
    /shape is invalid/u,
  );
  assert.throws(
    () => normalizeNpmPackReport([report, report], "@teslatlas/sdk"),
    /exactly one report/u,
  );
  assert.throws(
    () =>
      normalizeNpmPackReport({ "@teslatlas/sdk": { ...report, name: "wrong" } }, "@teslatlas/sdk"),
    /package identity is invalid/u,
  );
});
