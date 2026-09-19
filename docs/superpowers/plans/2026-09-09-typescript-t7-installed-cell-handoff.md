# TypeScript T7 Installed-Cell Handoff Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Produce a source-fixed, target-specific handoff for six TypeScript installed Node/browser matrix cells without launching a target or claiming acceptance.

**Architecture:** A public handoff binds immutable reviewed runner and SDK contract bytes, then names the per-cell private inputs the shared installed runner needs. `SessionInput` is generated only by `installed_registry.dispatch` after a selected host session opens and supplies a fresh descriptor.

**Tech Stack:** Python 3.11 matrix runner, Node 26.7.0, private JSON bindings, installed-host session schema.

**Spec:** `docs/development/PLAN.md` H3/H6; `docs/compatibility/matrix.json`; `tools/interop/matrix_runner/CONFIG.md`.

## Global Constraints

- Do not start macOS, Debian ARM64, or Debian AMD64 targets from this handoff.
- Keep each target's registration, environment, controller bundle, output root, session ID, and receipt separate and owner-only.
- Use only the source-fixed TypeScript Node/browser registry entries and their reviewed hashes.
- Do not put invitation, bearer, certificate, key, browser-state, or request bytes in the public handoff.
- Do not treat B1 packed-consumer receipts as T7 matrix proof.

---

### Task 1: Bind the fixed Node/browser runner contract

**Files:**
- Create: `docs/development/typescript-t7-installed-session-handoff-2026-09-09.md`
- Verify: `tools/interop/matrix_runner/typescript_installed.py`
- Verify: `tools/interop/matrix_runner/installed_registry.py`
- Test: `tools/interop/matrix_runner/test_typescript_installed.py`
- Test: `tools/interop/matrix_runner/test_installed_registry.py`

**Interfaces:**
- Consumes: `FIXED_INSTALLED_REGISTRY["typescript_node"]` and `FIXED_INSTALLED_REGISTRY["typescript_browser"]`.
- Produces: a redacted record of the six callbacks, contract hashes, SDK identity, actors, and target topology.

- [x] **Step 1: Run the fixed-registry tests**

Run `python3.11 -m unittest tools.interop.matrix_runner.test_typescript_installed tools.interop.matrix_runner.test_installed_registry` from `hub/`. Expected: eight tests pass, and both entries expose `build_session_input`, `launch_adapter`, `runtime_inventory`, `admit`, `build_supplement`, and `execution_logs`.

- [x] **Step 2: Record the reviewed source/contract identities**

Record both JSON/validator contract pairs, six Hub lane source hashes, the 81-member SDK archive digest, and the fixed Node version. Do not copy private values or paths.

### Task 2: Define the six independent private inputs without starting a target

**Files:**
- Create: `docs/development/typescript-t7-installed-session-handoff-2026-09-09.md`
- Verify: `tools/interop/matrix_runner/typescript_installed.py:297`
- Verify: `tools/interop/installed_hosts/session.schema.json`

**Interfaces:**
- Consumes: one target-specific installed-host registration and runner-opened host descriptor.
- Produces: one run-created owner-only `SessionInput` per cell.

- [x] **Step 1: Bind the exact cells and topology**

Record `typescript_node__macos_arm64`, `typescript_node__debian13_arm64`, `typescript_node__debian13_amd64`, `typescript_browser__macos_arm64`, `typescript_browser__debian13_arm64`, and `typescript_browser__debian13_amd64`. All use fixed local client execution and the runner-owned Unix broker.

- [x] **Step 2: Bind the private inputs**

Require distinct owner-only environment, installed-session config, registration inventory, host registration/controller bundle, descriptor, installed package root, scratch/output tree, and receipt. Browser adds remote-root, SSH configuration/alias, Playwright entry, and local-log bindings.

- [x] **Step 3: Preserve the run-created SessionInput boundary**

`build_session_input` derives its path from the fresh broker socket/session ID and stages the live certificate plus profile, scenario, and product bindings. Creating it early would fabricate those values or open a target session, both forbidden here.

### Task 3: Define evidence and start gate

**Files:**
- Create: `docs/development/typescript-t7-installed-session-handoff-2026-09-09.md`
- Modify: `docs/development/STATUS.json`

**Interfaces:**
- Consumes: source-fixed SessionInputs from Task 2 and explicit named-target start authority.
- Produces: a complete per-cell evidence checklist, with no result before launch.

- [x] **Step 1: Bind required behavior**

Map malformed/wrong, expired, replayed, and revoked credentials; claim/re-auth/rotation; restart/outage; pagination/cursors/ETag; cleanup; plus browser CORS and TLS validation.

- [x] **Step 2: Record the start preconditions**

Require a fresh approved target, observed native runtime and installed service, reviewed candidate Hub artifact, target-local installed SDK root matching the accepted archive, fresh owner-only registration/session/environment inputs, and browser-specific SSH/Playwright bindings.

- [x] **Step 3: Validate the handoff record**

Run `python3.11 -m json.tool docs/development/STATUS.json >/dev/null` and `git diff --check`. Expected: valid JSON and no whitespace errors.

## Self-Review

- H3's six-callback fixed boundary, H6's three target identities, and the matrix's six TypeScript cells are represented.
- The absent SessionInputs are an intentional run-created safety property, not a placeholder.
- Cell IDs, actors, target names, callbacks, artifact identity, and execution topology match the loaded matrix and fixed registry.
